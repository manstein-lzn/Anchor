//! Control one operator-installed user service, never a model-selected command.
use serde_json::{Value, json};
use std::{
    path::{Component, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{io::AsyncReadExt, process::Command, sync::Mutex};

const UNIT: &str = "anchor-codex-bridge.service";
const LIMIT: u64 = 4096;

pub(crate) struct CodexBridgeService {
    runtime: PathBuf,
    command: PathBuf,
    timeout: Duration,
    start_lock: Mutex<()>,
}

impl CodexBridgeService {
    pub(crate) fn from_runtime_dir(runtime: PathBuf) -> Result<Self, String> {
        if !runtime.is_absolute()
            || runtime
                .components()
                .any(|c| matches!(c, Component::ParentDir))
        {
            return Err("Bridge service requires an absolute runtime directory".into());
        }
        Ok(Self {
            runtime,
            command: "/usr/bin/systemctl".into(),
            timeout: Duration::from_secs(10),
            start_lock: Mutex::new(()),
        })
    }

    async fn run(&self, args: &[&str]) -> Result<String, String> {
        let mut child = Command::new(&self.command)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("LANG", "C")
            .env("XDG_RUNTIME_DIR", &self.runtime)
            .env(
                "DBUS_SESSION_BUS_ADDRESS",
                format!("unix:path={}/bus", self.runtime.display()),
            )
            .args(["--user", "--no-pager", "--no-ask-password"])
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| "Bridge service controller could not start".to_owned())?;
        let stdout = child
            .stdout
            .take()
            .ok_or("Bridge service controller has no output")?;
        let result = tokio::time::timeout(self.timeout, async {
            let mut bytes = Vec::new();
            stdout
                .take(LIMIT + 1)
                .read_to_end(&mut bytes)
                .await
                .map_err(|_| "Bridge service state could not be read".to_owned())?;
            if bytes.len() as u64 > LIMIT {
                return Err("Bridge service controller output exceeded limit".to_owned());
            }
            let status = child
                .wait()
                .await
                .map_err(|_| "Bridge service controller did not finish".to_owned())?;
            if !status.success() {
                return Err(
                    "Bridge user-service operation failed; operator inspection is required"
                        .to_owned(),
                );
            }
            String::from_utf8(bytes)
                .map_err(|_| "Bridge service state is not valid text".to_owned())
        })
        .await;
        match result {
            Ok(value) => value,
            Err(_) => Err("Bridge service operation timed out; outcome unknown, inspect status before deciding, do not blindly retry".into()),
        }
    }

    pub(crate) async fn status(&self) -> Result<Value, String> {
        let output = self
            .run(&[
                "show",
                UNIT,
                "--property=LoadState,ActiveState,SubState,MainPID,Result",
            ])
            .await?;
        let fields: std::collections::BTreeMap<_, _> = output
            .lines()
            .filter_map(|line| line.split_once('='))
            .collect();
        let field = |name| {
            fields
                .get(name)
                .copied()
                .filter(|s| {
                    !s.is_empty()
                        && s.len() <= 64
                        && s.bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                })
                .ok_or_else(|| format!("Bridge service state lacks valid {name}"))
        };
        Ok(
            json!({"unit":UNIT, "load_state":field("LoadState")?, "active_state":field("ActiveState")?,
            "sub_state":field("SubState")?, "main_pid":field("MainPID")?.parse::<u32>().map_err(|_| "Bridge service PID is invalid")?,
            "result":field("Result")?}),
        )
    }

    pub(crate) async fn start(&self) -> Result<Value, String> {
        let _guard = tokio::time::timeout(self.timeout, self.start_lock.lock())
            .await
            .map_err(|_| "Bridge startup already being inspected; check status")?;
        let before = self.status().await?;
        if before["load_state"] != "loaded" {
            return Err(
                "Existing Bridge user service is not loaded; installation is an operator action"
                    .into(),
            );
        }
        if before["active_state"] == "active" {
            return Ok(json!({"action":"already_active", "state":before}));
        }
        if before["active_state"] == "deactivating" {
            return Err("Bridge service is stopping; do not override the in-progress stop".into());
        }
        self.run(&["start", UNIT]).await?;
        let after = self.status().await?;
        Ok(json!({"action":"start_requested", "state":after, "mcp_connection_verified":false}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn fixture(root: &std::path::Path) -> CodexBridgeService {
        let script = root.join("systemctl-fixture");
        std::fs::write(&script, r#"#!/bin/sh
set -eu
test "$1" = --user; test "$2" = --no-pager; test "$3" = --no-ask-password
test "$5" = anchor-codex-bridge.service
test "$DBUS_SESSION_BUS_ADDRESS" = "unix:path=$XDG_RUNTIME_DIR/bus"
case "$4" in
show)
  test "$6" = --property=LoadState,ActiveState,SubState,MainPID,Result
  state=inactive; pid=0
  if test -f "$XDG_RUNTIME_DIR/active"; then state=active; pid=123; fi
  printf 'LoadState=loaded\nActiveState=%s\nSubState=running\nMainPID=%s\nResult=success\n' "$state" "$pid";;
start)
  printf 'start\n' >> "$XDG_RUNTIME_DIR/calls"
  touch "$XDG_RUNTIME_DIR/active";;
*) exit 9;;
esac
"#).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut service = CodexBridgeService::from_runtime_dir(root.into()).unwrap();
        service.command = script;
        service
    }

    #[tokio::test]
    async fn fixed_service_starts_once_and_running_service_is_not_restarted() {
        let root = tempfile::tempdir().unwrap();
        let service = fixture(root.path());
        assert_eq!(service.status().await.unwrap()["active_state"], "inactive");
        let (a, b) = tokio::join!(service.start(), service.start());
        let actions = [a.unwrap()["action"].clone(), b.unwrap()["action"].clone()];
        assert!(actions.contains(&json!("start_requested")));
        assert!(actions.contains(&json!("already_active")));
        assert_eq!(
            std::fs::read_to_string(root.path().join("calls")).unwrap(),
            "start\n"
        );
        assert_eq!(service.status().await.unwrap()["main_pid"], 123);
    }

    #[tokio::test]
    async fn controller_timeout_is_unknown_and_output_is_bounded() {
        let root = tempfile::tempdir().unwrap();
        let mut service = fixture(root.path());
        service.timeout = Duration::from_millis(80);
        std::fs::write(&service.command, "#!/bin/sh\nexec sleep 10\n").unwrap();
        assert!(
            service
                .status()
                .await
                .unwrap_err()
                .contains("outcome unknown")
        );
        std::fs::write(&service.command, "#!/bin/sh\nhead -c 5000 /dev/zero\n").unwrap();
        assert!(
            service
                .status()
                .await
                .unwrap_err()
                .contains("exceeded limit")
        );
        assert!(CodexBridgeService::from_runtime_dir("relative".into()).is_err());
    }
}
