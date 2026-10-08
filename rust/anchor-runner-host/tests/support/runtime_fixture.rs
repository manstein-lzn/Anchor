//! Isolated Host fixtures shared by Goose tests. No dotenv or inherited credentials.
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

pub fn wait_until(description: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if condition() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {description}"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

pub struct Host {
    pub root: tempfile::TempDir,
    binary: PathBuf,
    allowed_commands: String,
}

impl Host {
    pub fn new(graph: &Value) -> Self {
        let root = tempfile::tempdir().unwrap();
        let bundle = root.path().join("bundle");
        fs::create_dir_all(root.path().join("state")).unwrap();
        fs::create_dir_all(root.path().join("work")).unwrap();
        fs::create_dir_all(&bundle).unwrap();
        fs::write(bundle.join("graph.json"), graph.to_string()).unwrap();
        fs::write(
            bundle.join("manifest.json"),
            r#"{"format":1,"graph":"graph.json","plugins":[]}"#,
        )
        .unwrap();
        let binary = root.path().join("anchor-runner-host");
        if fs::hard_link(env!("CARGO_BIN_EXE_anchor-runner-host"), &binary).is_err() {
            fs::copy(env!("CARGO_BIN_EXE_anchor-runner-host"), &binary).unwrap();
        }
        Self {
            root,
            binary,
            allowed_commands: "sh,cat,git,true".into(),
        }
    }

    #[allow(dead_code)]
    pub fn with_allowed_commands(mut self, commands: &str) -> Self {
        self.allowed_commands = commands.to_owned();
        self
    }

    fn process(&self) -> Command {
        let mut process = Command::new(&self.binary);
        process
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("LANG", "C.UTF-8")
            .env("TZ", "UTC")
            .env("ANCHOR_RUNNER_STATE_ROOT", self.root.path().join("state"))
            .env(
                "ANCHOR_RUNNER_WORKSPACE_ROOT",
                self.root.path().join("work"),
            )
            .env("ANCHOR_RUNNER_BUNDLE_ROOT", self.root.path().join("bundle"))
            .env("ANCHOR_RUNNER_CATALOG_ROOT", self.root.path())
            .env("ANCHOR_RUNNER_GRAPH_NAME", "fixture")
            .env(
                "ANCHOR_RUNNER_SCHEDULES_PATH",
                self.root.path().join("schedules.json"),
            )
            .env("ANCHOR_RUNNER_ALLOWED_COMMANDS", &self.allowed_commands)
            .current_dir(self.root.path());
        process
    }

    pub fn record(&self) -> Value {
        self.record_for("fixture")
    }

    pub fn record_for(&self, run: &str) -> Value {
        read_json(
            self.root
                .path()
                .join("state/runs")
                .join(format!("{run}.json")),
        )
    }

    pub fn serve_without_model(&self) -> HttpHost {
        self.serve_process(self.process())
    }

    fn serve_process(&self, mut process: Command) -> HttpHost {
        let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = socket.local_addr().unwrap();
        drop(socket);
        let log = self.root.path().join("http-host.log");
        let child = process
            .arg("serve")
            .env("ANCHOR_RUNNER_LISTEN", address.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(fs::File::create(&log).unwrap())
            .spawn()
            .unwrap();
        let mut server = HttpHost {
            child,
            url: format!("http://{address}"),
            log,
            runtime: tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap(),
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap(),
        };
        wait_until("HTTP Host startup", || {
            assert!(
                server.child.try_wait().unwrap().is_none(),
                "Host startup failed: {}",
                fs::read_to_string(&server.log).unwrap()
            );
            server
                .try_request("GET", "/health", None)
                .is_ok_and(|(status, _)| status == 200)
        });
        server
    }

    pub fn artifact(&self, record: &Value, node: &str) -> PathBuf {
        self.root
            .path()
            .join("state/artifacts")
            .join(record["results"][node][0]["commit"]["id"].as_str().unwrap())
    }

    pub fn file(&self, record: &Value, node: &str, name: &str) -> Vec<u8> {
        fs::read(self.artifact(record, node).join("files").join(name)).unwrap()
    }

    pub fn workspace_files(&self, run: &str, name: &str) -> Vec<Vec<u8>> {
        let root = self.root.path().join("work").join(run);
        let mut files = Vec::new();
        visit_files(&root, &root, &mut |path, _| {
            if path.file_name().is_some_and(|file| file == name) {
                files.push(fs::read(path).unwrap());
            }
        });
        files
    }
}

pub struct HttpHost {
    child: Child,
    url: String,
    log: PathBuf,
    runtime: tokio::runtime::Runtime,
    client: reqwest::Client,
}

impl Drop for Host {
    fn drop(&mut self) {
        if thread::panicking() {
            self.root.disable_cleanup(true);
            eprintln!(
                "failed Runtime fixture retained: {}",
                self.root.path().display()
            );
        }
    }
}

impl HttpHost {
    fn try_request(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
    ) -> Result<(u16, Value), reqwest::Error> {
        self.try_request_with_headers(method, path, body, &[])
    }

    fn try_request_with_headers(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        headers: &[(&str, &str)],
    ) -> Result<(u16, Value), reqwest::Error> {
        self.runtime.block_on(async {
            let mut request = self
                .client
                .request(method.parse().unwrap(), format!("{}{path}", self.url));
            for (name, value) in headers {
                request = request.header(*name, *value);
            }
            if let Some(body) = body {
                request = request.json(body);
            }
            let response = request.send().await?;
            let status = response.status().as_u16();
            let bytes = response.bytes().await?;
            let value = if bytes.is_empty() {
                Value::Null
            } else {
                serde_json::from_slice(&bytes).unwrap()
            };
            Ok((status, value))
        })
    }

    pub fn request(&self, method: &str, path: &str, body: Option<&Value>) -> (u16, Value) {
        self.try_request(method, path, body).unwrap()
    }

    pub fn request_with_headers(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
    ) -> (u16, Value) {
        self.try_request_with_headers(method, path, None, headers)
            .unwrap()
    }

    pub fn events(&self, path: &str, last_event: Option<u64>) -> (u16, String) {
        self.runtime.block_on(async {
            let mut request = self.client.get(format!("{}{path}", self.url));
            if let Some(last_event) = last_event {
                request = request.header("Last-Event-ID", last_event);
            }
            let response = request.send().await.unwrap();
            let status = response.status().as_u16();
            (status, response.text().await.unwrap())
        })
    }

    pub fn trigger(&self) -> String {
        let (status, accepted) =
            self.request("POST", "/trigger", Some(&json!({"graph":"fixture"})));
        assert_eq!(status, 202, "{accepted}");
        accepted["run"].as_str().unwrap().to_owned()
    }

    pub fn wait_status(&self, run: &str, expected: &str) -> Value {
        let mut detail = Value::Null;
        wait_until(expected, || {
            let (status, saved) = self.request("GET", &format!("/runs/{run}"), None);
            assert_eq!(status, 200, "{saved}");
            detail = saved;
            assert!(
                !matches!(
                    detail["state"]["status"].as_str(),
                    Some("failed" | "budget_stopped")
                ),
                "unexpected Run failure: {detail}"
            );
            detail["state"]["status"] == expected && detail["active"] == false
        });
        detail
    }
}

impl Drop for HttpHost {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub fn read_json(path: impl AsRef<Path>) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn visit_files(root: &Path, directory: &Path, visitor: &mut impl FnMut(&Path, &str)) {
    if !directory.exists() {
        return;
    }
    let mut entries = fs::read_dir(directory)
        .unwrap()
        .map(Result::unwrap)
        .collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        if entry.file_type().unwrap().is_dir() {
            if entry.file_name() != "git-view" && entry.file_name() != ".git" {
                visit_files(root, &path, visitor);
            }
        } else if entry.file_type().unwrap().is_file() {
            visitor(&path, path.strip_prefix(root).unwrap().to_str().unwrap());
        }
    }
}
