//! Fixtures shared by Host unit and HTTP tests.

use std::{
    env,
    ffi::{OsStr, OsString},
};

/// Restore process configuration on every exit path. Callers hold PROCESS_ENV_LOCK.
#[derive(Default)]
pub(crate) struct Environment(pub(crate) Vec<(&'static str, Option<OsString>)>);

impl Environment {
    pub(crate) fn cleared(names: &[&'static str]) -> Self {
        let mut environment = Self::default();
        for name in names {
            environment.remove(name);
        }
        environment
    }

    pub(crate) fn set(&mut self, name: &'static str, value: impl AsRef<OsStr>) {
        self.0.push((name, env::var_os(name)));
        unsafe { env::set_var(name, value) };
    }

    pub(crate) fn remove(&mut self, name: &'static str) {
        self.0.push((name, env::var_os(name)));
        unsafe { env::remove_var(name) };
    }
}

impl Drop for Environment {
    fn drop(&mut self) {
        for (name, value) in self.0.iter().rev() {
            unsafe {
                match value {
                    Some(value) => env::set_var(name, value),
                    None => env::remove_var(name),
                }
            }
        }
    }
}

pub(crate) struct EnvVar {
    _environment: Environment,
}

impl EnvVar {
    pub(crate) fn set(name: &'static str, value: impl AsRef<OsStr>) -> Self {
        let mut environment = Environment::default();
        environment.set(name, value);
        Self {
            _environment: environment,
        }
    }

    pub(crate) fn unset(name: &'static str) -> Self {
        Self {
            _environment: Environment::cleared(&[name]),
        }
    }
}

pub(crate) fn idle_assistant_environment(root: &std::path::Path) -> Environment {
    use sha2::Digest;
    let mut environment = Environment::cleared(&[
        "ANCHOR_RUNNER_LOCAL_INPUTS_ROOT",
        "ANCHOR_RUNNER_FAKE_PLUGINS",
        "ANCHOR_GOOSE_RELAY_BINARY",
        "ANCHOR_MODEL_ALIASES",
    ]);
    let binary = root.join("unstarted-goose");
    let bytes = b"Assistant handover fixture never starts an Agent";
    std::fs::write(&binary, bytes).unwrap();
    environment.set("ANCHOR_GOOSE_BINARY", &binary);
    environment.set(
        "ANCHOR_GOOSE_BINARY_SHA256",
        format!("{:x}", sha2::Sha256::digest(bytes)),
    );
    environment.set("ANCHOR_GOOSE_LOCAL_NETWORK", "0");
    environment.set("ANCHOR_GOOSE_ALLOW_SHARED_NETWORK", "1");
    environment.set("ANCHOR_RUNNER_WORKSPACE_ROOT", root.join("workspaces"));
    environment.set("ANCHOR_RUNNER_ALLOWED_COMMANDS", "true");
    environment.set("ANCHOR_RUNNER_AGENT_RUNTIME", "goose");
    environment.set("ANCHOR_MODEL_URL", "http://127.0.0.1:1/v1");
    environment.set("ANCHOR_MODEL_API_KEY", "handover-fixture");
    environment.set("ANCHOR_MODEL_NAME", "fixture");
    environment.set("ANCHOR_MODEL_WIRE_API", "chat");
    environment.set("ANCHOR_MODEL_DIRECT", "0");
    environment
}
pub(crate) fn resolve_task_result_silently(
    store: &anchor_platform_session::SessionStore,
    owner: &str,
    session: &str,
    run: &str,
    key: &str,
) {
    use anchor_platform_session::{AssistantInput, TaskResultResolution};
    let AssistantInput::Task(input) = store
        .get_assistant_input(owner, session, run, key)
        .unwrap()
        .unwrap()
    else {
        panic!("expected task input");
    };
    if store
        .task_result_resolution(owner, session, &input.result.id)
        .unwrap()
        .is_none()
    {
        store
            .resolve_task_result(
                owner,
                session,
                run,
                key,
                &TaskResultResolution::Silent {
                    reason: "This fixture consumes the result without a channel notification"
                        .into(),
                },
            )
            .unwrap();
    }
}
