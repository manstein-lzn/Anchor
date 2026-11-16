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
