use crate::plugin_library::Catalog;
use anchor_graph_host::{ChannelDefinition, FileGraphBundleLoader};
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
    runtime::Handle,
    sync::{Notify, watch},
    task::JoinHandle,
    time::{Instant, sleep},
};

pub(crate) mod management;
pub(crate) mod status;

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

#[derive(Clone)]
struct LaunchSpec {
    platform: String,
    executable: PathBuf,
    plugin_dir: PathBuf,
    environment: BTreeMap<String, OsString>,
    descriptor: PathBuf,
    control_socket: PathBuf,
    resource_guard: Option<ChannelResourceGuard>,
}

#[derive(Clone)]
struct ChannelResourceGuard {
    bundle_root: PathBuf,
    library_root: PathBuf,
    definition: ChannelDefinition,
}

impl ChannelResourceGuard {
    fn valid(&self) -> bool {
        self.bundle_root.join("manifest.json").is_file()
            && self
                .library_root
                .join("plugins")
                .join(&self.definition.plugin)
                .join("plugin.json")
                .is_file()
            && Catalog::new(&self.library_root)
                .channels(&self.definition.plugin)
                .is_ok_and(|channels| channels.contains(&self.definition))
    }
}

pub(crate) struct ManagedChannelLaunch {
    /// The bot Graph; the gateway's state and callback routes are its own.
    pub(crate) graph: String,
    pub(crate) plugin: String,
    pub(crate) platform: String,
    pub(crate) credentials: BTreeMap<String, String>,
    pub(crate) users: Vec<String>,
    pub(crate) send_users: Vec<String>,
    pub(crate) callback_key: String,
}

/// One bot Graph's gateway process.
pub(crate) struct ChannelSupervisor {
    shutdown: std::sync::Arc<Notify>,
    task: Option<JoinHandle<()>>,
    fallback: watch::Receiver<status::ConnectionStatus>,
    cleanup_paths: Vec<(PathBuf, PathBuf)>,
}

/// Where a bot Graph's gateway keeps its ledger, profile, lock and control
/// descriptor. Nothing outside this directory is shared between bots. The
/// gateway binds `control.sock` inside it and a Unix socket path is limited to
/// [`MAX_SOCKET_PATH`] bytes, so the directory is named by a fixed-length digest
/// of the Graph rather than by the Graph name, which may be 64 characters.
pub(crate) fn state_directory(state_root: &Path, graph: &str) -> PathBuf {
    use sha2::{Digest, Sha256};
    let digest = format!("{:x}", Sha256::digest(graph.as_bytes()));
    state_root.join("channels/graphs").join(&digest[..16])
}

/// `sun_path` holds 108 bytes including the terminating NUL.
const MAX_SOCKET_PATH: usize = 107;

/// The control descriptor a running bot Graph's gateway publishes.
pub(crate) fn descriptor_path(state_root: &Path, graph: &str) -> PathBuf {
    state_directory(state_root, graph).join("control.json")
}

impl ChannelSupervisor {
    pub(crate) async fn start_managed(
        host_runtime: &Handle,
        bundle_root: &Path,
        library_root: &Path,
        state_root: &Path,
        listen: &str,
        managed: ManagedChannelLaunch,
    ) -> io::Result<Self> {
        let channel = discover(bundle_root, library_root)?
            .into_iter()
            .find(|channel| {
                channel.definition.plugin == managed.plugin
                    && channel.definition.platform == managed.platform
            })
            .ok_or_else(|| io::Error::other("the Graph has no matching admitted channel"))?;
        let definition = &channel.definition;
        let mut environment = BTreeMap::new();
        environment.insert("PATH".into(), parent_environment("PATH"));
        environment.insert("HOME".into(), parent_environment("HOME"));
        for required in &definition.required_environment {
            match managed.credentials.get(required) {
                Some(value) if !value.trim().is_empty() && !value.chars().any(char::is_control) => {
                    environment.insert(required.clone(), value.clone().into());
                }
                Some(_) => {
                    return Err(io::Error::other(format!(
                        "channel credential {required} is blank or contains control characters"
                    )));
                }
                None => {
                    return Err(io::Error::other(format!(
                        "required channel credential {required} is not configured"
                    )));
                }
            }
        }
        let state_dir = state_directory(state_root, &managed.graph);
        private_directory(&state_dir)?;
        let descriptor = state_dir.join("control.json");
        let control_socket = state_dir.join("control.sock");
        if control_socket.as_os_str().len() > MAX_SOCKET_PATH {
            return Err(io::Error::other(format!(
                "the gateway control socket path exceeds {MAX_SOCKET_PATH} bytes; use a shorter Host state root"
            )));
        }
        let control_token = random_token("anchor-channel-control")?;
        let base = channel_base_url(listen, &managed.graph);
        environment.insert("WECOM_CHANNEL_STATE".into(), state_dir.clone().into());
        environment.insert(
            "ANCHOR_CHANNEL_WEBHOOK_URL".into(),
            format!("{base}/events").into(),
        );
        environment.insert(
            "ANCHOR_CHANNEL_PROGRESS_URL".into(),
            format!("{base}/progress").into(),
        );
        environment.insert("ANCHOR_API_KEY".into(), managed.callback_key.into());
        environment.insert("ANCHOR_CHANNEL_CONTROL_TOKEN".into(), control_token.into());
        environment.insert(
            "ANCHOR_CHANNEL_CONTROL_DESCRIPTOR".into(),
            descriptor.clone().into(),
        );
        environment.insert(
            "ANCHOR_CHANNEL_CONTROL_SOCKET".into(),
            control_socket.clone().into(),
        );
        // Operator tuning passes through; who may talk to the bot is the
        // binding's, never the process environment's.
        for name in [
            "ANCHOR_WECOM_WS_URL",
            "ANCHOR_WECOM_CONNECT_MS",
            "ANCHOR_WECOM_ACK_MS",
            "ANCHOR_WECOM_HEARTBEAT_MS",
            "ANCHOR_WECOM_RECONNECT_MS",
            "ANCHOR_WECOM_RECONNECT_MAX_MS",
            "ANCHOR_WECOM_ACK_DELAY_MS",
            "ANCHOR_WECOM_RECOVERY_WINDOW_MS",
            "ANCHOR_WECOM_DEBUG",
            "ANCHOR_WECOM_PROGRESS",
            "ANCHOR_CHANNEL_WEBHOOK_TIMEOUT_MS",
        ] {
            if let Some(value) = env::var_os(name) {
                environment.insert(name.into(), value);
            }
        }
        environment.insert("ANCHOR_WECOM_USERS".into(), managed.users.join(",").into());
        environment.insert(
            "ANCHOR_WECOM_SEND_USERS".into(),
            managed.send_users.join(",").into(),
        );
        let launch = LaunchSpec {
            platform: definition.platform.clone(),
            executable: channel.executable.clone(),
            plugin_dir: channel.plugin_dir.clone(),
            environment,
            descriptor: descriptor.clone(),
            control_socket: control_socket.clone(),
            resource_guard: Some(ChannelResourceGuard {
                bundle_root: bundle_root.to_path_buf(),
                library_root: library_root.to_path_buf(),
                definition: definition.clone(),
            }),
        };
        let shutdown = std::sync::Arc::new(Notify::new());
        let (status, fallback) = watch::channel(status::ConnectionStatus::Connecting);
        // A Pilot Turn owns a temporary runtime. Its caller may start a bot,
        // but the Host runtime must own both supervision and child IO.
        let task = host_runtime.spawn(supervise(vec![launch], shutdown.clone(), status));
        Ok(Self {
            shutdown,
            task: Some(task),
            fallback,
            cleanup_paths: vec![(descriptor, control_socket)],
        })
    }

    fn is_running(&self) -> bool {
        self.task.as_ref().is_some_and(|task| !task.is_finished())
    }

    fn fallback_status(&self) -> status::ConnectionStatus {
        if self.is_running() {
            *self.fallback.borrow()
        } else {
            status::ConnectionStatus::Unavailable
        }
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

fn discover(bundle_root: &Path, library_root: &Path) -> io::Result<Vec<DiscoveredChannel>> {
    // The Graph bundle references its Plugins; the code — and the channel
    // declaration and entrypoint — live in the Library. A bundle carries no Plugin
    // bytes, so resolving them from the bundle root would find nothing (or, worse,
    // a stale copy).
    let bundle = FileGraphBundleLoader::new(bundle_root)
        .with_library_root(library_root)
        .load()
        .map_err(|error| io::Error::other(format!("channel Graph admission failed: {error}")))?;
    let ids = bundle
        .snapshot
        .nodes
        .iter()
        .flat_map(|node| node.plugins.iter().map(|reference| reference.id.clone()))
        .collect::<BTreeSet<_>>();
    let catalog = Catalog::new(library_root);
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

async fn supervise(
    launches: Vec<LaunchSpec>,
    shutdown: std::sync::Arc<Notify>,
    status: watch::Sender<status::ConnectionStatus>,
) {
    let mut children = (0..launches.len())
        .map(|_| None::<Child>)
        .collect::<Vec<_>>();
    let mut restart_at = vec![Instant::now(); launches.len()];
    loop {
        let now = Instant::now();
        for (index, launch) in launches.iter().enumerate() {
            // A saved binding does not preserve deleted resources. Stop the
            // existing transport and leave an unavailable observation; a new
            // enable must pass Graph/Plugin admission again.
            if launch
                .resource_guard
                .as_ref()
                .is_some_and(|guard| !guard.valid())
            {
                status.send_replace(status::ConnectionStatus::Unavailable);
                if let Some(mut child) = children[index].take() {
                    stop_child(&mut child).await;
                }
                remove_launch_control_paths(launch);
                continue;
            }
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
                if children[index].is_none() {
                    status.send_replace(status::ConnectionStatus::Reconnecting);
                } else if launch.descriptor.is_file() {
                    // Once published, a missing/unresponsive control endpoint
                    // is unavailable; do not keep calling it initial startup.
                    status.send_replace(status::ConnectionStatus::Unavailable);
                }
            }
            if children[index].is_none() && now >= restart_at[index] {
                remove_launch_control_paths(launch);
                match spawn(launch) {
                    Ok(child) => {
                        children[index] = Some(child);
                        status.send_replace(status::ConnectionStatus::Connecting);
                    }
                    Err(error) => {
                        status.send_replace(status::ConnectionStatus::Reconnecting);
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

fn channel_base_url(listen: &str, graph: &str) -> String {
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
    format!("http://127.0.0.1:{port}/channels/{graph}")
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
        let bindings = Catalog::new(root.path()).resolve(&plugin_ids).unwrap();
        for binding in bindings {
            summaries.push(json!({"id": binding.id}));
        }
        fs::write(
            root.path().join("manifest.json"),
            json!({"format": 1, "graph": "graph.json", "plugins": summaries}).to_string(),
        )
        .unwrap();
        root
    }

    fn refresh_manifest(root: &std::path::Path, plugin_ids: &[&str]) {
        let bindings = Catalog::new(root)
            .resolve(
                &plugin_ids
                    .iter()
                    .map(|id| (*id).to_owned())
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        let summaries = bindings
            .into_iter()
            .map(|binding| json!({"id": binding.id}))
            .collect::<Vec<_>>();
        fs::write(
            root.join("manifest.json"),
            json!({"format": 1, "graph": "graph.json", "plugins": summaries}).to_string(),
        )
        .unwrap();
    }

    #[test]
    fn each_bot_graph_has_its_own_callback_routes_and_state() {
        assert_eq!(
            channel_base_url("127.0.0.1:8077", "sales-bot"),
            "http://127.0.0.1:8077/channels/sales-bot"
        );
        assert_eq!(
            descriptor_path(Path::new("/state"), "sales-bot"),
            Path::new("/state/channels/graphs/e12aa0cd6c08ef3d/control.json")
        );
        let longest = "g".repeat(64);
        assert_eq!(
            state_directory(Path::new("/state"), &longest),
            state_directory(Path::new("/state"), &longest)
        );
        assert_ne!(
            state_directory(Path::new("/state"), "sales-bot"),
            state_directory(Path::new("/state"), "sales-bot2")
        );
        assert_eq!(
            state_directory(Path::new("/state"), &longest)
                .as_os_str()
                .len(),
            "/state/channels/graphs/".len() + 16
        );
    }

    fn launch(credentials: BTreeMap<String, String>) -> ManagedChannelLaunch {
        ManagedChannelLaunch {
            graph: "bot".into(),
            plugin: "mounted".into(),
            platform: "wecom".into(),
            credentials,
            users: vec!["member".into()],
            send_users: vec!["member".into()],
            callback_key: "callback".into(),
        }
    }

    /// The standard deployment's three roots: the Graph bundle is one directory,
    /// the Library — with the Plugin's home — is another, and the bundle carries no
    /// Plugin bytes at all. Discovery must resolve the channel from the Library.
    #[test]
    fn discovers_a_channel_from_the_library_when_the_bundle_carries_no_plugins() {
        let root = tempfile::tempdir().unwrap();
        let library = root.path().join("library");
        let bundle = root.path().join("catalog/used");
        fs::create_dir_all(&bundle).unwrap();
        // The Plugin, with its channel declaration and entrypoint, lives once.
        let plugin = library.join("plugins/wecom");
        fs::create_dir_all(&plugin).unwrap();
        fs::write(
            plugin.join("plugin.json"),
            json!({"name":"WeCom","skills":"skills/"}).to_string(),
        )
        .unwrap();
        fs::create_dir_all(plugin.join("skills")).unwrap();
        fs::write(plugin.join("skills/SKILL.md"), "code").unwrap();
        fs::write(
            plugin.join("channel.json"),
            json!({"platform":"wecom","transport":"websocket","entrypoint":"bin/gateway"})
                .to_string(),
        )
        .unwrap();
        fs::create_dir_all(plugin.join("bin")).unwrap();
        fs::write(plugin.join("bin/gateway"), "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(
            plugin.join("bin/gateway"),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        fs::write(
            bundle.join("graph.json"),
            json!({
                "objective": "channel",
                "entry": "worker",
                "agents": {"worker": {"model": "fixture", "instructions": "work"}},
                "ops": {},
                "nodes": [{"id": "worker", "agent": "worker", "plugins": ["wecom"]}],
                "edges": []
            })
            .to_string(),
        )
        .unwrap();
        fs::write(
            bundle.join("manifest.json"),
            json!({"format":1,"graph":"graph.json","plugins":[{"id":"wecom"}]}).to_string(),
        )
        .unwrap();
        assert!(!bundle.join("plugins").exists());

        let discovered = discover(&bundle, &library).unwrap();
        assert_eq!(discovered.len(), 1, "{discovered:?}");
        assert_eq!(discovered[0].definition.platform, "wecom");
        assert_eq!(
            discovered[0].executable,
            plugin.join("bin/gateway").canonicalize().unwrap(),
            "the entrypoint comes from the Library"
        );
        // With the bundle root as the Library root there is nothing to find: this
        // is the shape that used to make the host fail to start.
        let missing = discover(&bundle, &bundle).unwrap_err();
        assert!(
            missing
                .to_string()
                .contains("Plugin `wecom` is not installed in the Library at"),
            "resolving from the bundle must name the Plugin and the root it looked in: {missing}"
        );
    }

    #[test]
    fn discovers_only_channels_mounted_by_the_graph() {
        let root = bundle(&[("mounted", "wecom", "bin/gateway")]);
        let discovered = discover(root.path(), root.path()).unwrap();
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
        let error =
            discover(root.path(), root.path()).expect_err("duplicate channel must fail closed");
        assert!(
            error
                .to_string()
                .contains("conflicting channel declarations")
        );
    }

    #[tokio::test]
    async fn missing_or_blank_required_channel_environment_fails_before_spawn() {
        let root = bundle(&[("mounted", "wecom", "bin/gateway")]);
        let channel = root.path().join("plugins/mounted/channel.json");
        let mut value: Value = serde_json::from_slice(&fs::read(&channel).unwrap()).unwrap();
        value["required_environment"] = json!(["ANCHOR_CHANNEL_TEST_REQUIRED"]);
        fs::write(&channel, value.to_string()).unwrap();
        refresh_manifest(root.path(), &["mounted"]);
        for value in [None, Some(""), Some("   ")] {
            let credentials = value
                .map(|value| {
                    BTreeMap::from([("ANCHOR_CHANNEL_TEST_REQUIRED".to_owned(), value.to_owned())])
                })
                .unwrap_or_default();
            let result = ChannelSupervisor::start_managed(
                &Handle::current(),
                root.path(),
                root.path(),
                &root.path().join("state"),
                "127.0.0.1:8077",
                launch(credentials),
            )
            .await;
            let error = match result {
                Err(error) => error,
                Ok(_) => panic!("missing or blank channel credential must fail closed: {value:?}"),
            };
            assert!(error.to_string().contains("ANCHOR_CHANNEL_TEST_REQUIRED"));
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
        let mut supervisor = ChannelSupervisor::start_managed(
            &Handle::current(),
            root.path(),
            root.path(),
            &state,
            "127.0.0.1:8077",
            launch(BTreeMap::new()),
        )
        .await
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
        let descriptor = state_directory(&state, "bot").join("control.json");
        let socket = state_directory(&state, "bot").join("control.sock");
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
        let mut supervisor = ChannelSupervisor::start_managed(
            &Handle::current(),
            root.path(),
            root.path(),
            &state,
            "127.0.0.1:8077",
            launch(BTreeMap::new()),
        )
        .await
        .unwrap();
        tokio::time::sleep(POLL_INTERVAL * 2 + restart_delay() * 2).await;
        supervisor.shutdown().await;
        assert!(!descriptor.exists());
        assert!(!socket.exists());
    }
}
