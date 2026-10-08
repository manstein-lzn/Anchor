use anchor_runtime::{ToolError, graph::InvocationKey};
use sha2::{Digest, Sha256};
use std::future::Future;

#[derive(Clone, Debug)]
pub(crate) struct GooseToolIdentity {
    pub key: InvocationKey,
    pub session: String,
    pub tool_call: String,
}

tokio::task_local! {
    static TOOL_IDENTITY: Option<GooseToolIdentity>;
}

pub(crate) async fn scope<F: Future>(identity: Option<GooseToolIdentity>, future: F) -> F::Output {
    TOOL_IDENTITY.scope(identity, future).await
}

fn invalid_identity() -> ToolError {
    ToolError::Failed(
        "missing or invalid host-verified Goose tool identity; no message was sent".into(),
    )
}

fn valid_identifier(value: &str, maximum: usize) -> bool {
    !value.trim().is_empty() && value.len() <= maximum && !value.chars().any(char::is_control)
}

pub(crate) fn channel_request_id(key: &InvocationKey) -> Result<String, ToolError> {
    TOOL_IDENTITY
        .try_with(|identity| {
            let identity = identity.as_ref().ok_or_else(invalid_identity)?;
            if identity.key != *key
                || !valid_identifier(&key.run_id, 512)
                || !valid_identifier(&key.graph_digest, 512)
                || !valid_identifier(&key.node_id, 512)
                || key.invocation == 0
                || !valid_identifier(&identity.session, 128)
                || !valid_identifier(&identity.tool_call, 512)
            {
                return Err(invalid_identity());
            }
            let binding = serde_json::to_vec(&(
                key,
                &identity.session,
                &identity.tool_call,
                crate::channel_tools::SEND_TOOL,
            ))
            .map_err(|_| invalid_identity())?;
            let mut digest = Sha256::new();
            digest.update(b"anchor-goose-channel-send-v1\0");
            digest.update(binding);
            Ok(format!("channel-send-{:x}", digest.finalize()))
        })
        .map_err(|_| invalid_identity())?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(tool_call: &str) -> GooseToolIdentity {
        GooseToolIdentity {
            key: InvocationKey {
                run_id: "run".into(),
                graph_digest: "digest".into(),
                node_id: "worker".into(),
                invocation: 1,
            },
            session: "goose-session".into(),
            tool_call: tool_call.into(),
        }
    }

    async fn request_id(identity: GooseToolIdentity) -> String {
        let key = identity.key.clone();
        scope(Some(identity), async move {
            tokio::task::yield_now().await;
            channel_request_id(&key).unwrap()
        })
        .await
    }

    #[tokio::test]
    async fn identity_is_stable_and_bound_to_every_component() {
        let original = identity("native-call-1");
        let expected = request_id(original.clone()).await;
        assert_eq!(expected, request_id(original.clone()).await);
        assert_eq!(expected.len(), "channel-send-".len() + 64);
        let mut alternatives = Vec::new();
        let mut changed = original.clone();
        changed.tool_call = "native-call-2".into();
        alternatives.push(changed);
        let mut changed = original.clone();
        changed.session = "other-session".into();
        alternatives.push(changed);
        let mut changed = original.clone();
        changed.key.run_id = "other-run".into();
        alternatives.push(changed);
        let mut changed = original.clone();
        changed.key.graph_digest = "other-digest".into();
        alternatives.push(changed);
        let mut changed = original.clone();
        changed.key.node_id = "other-node".into();
        alternatives.push(changed);
        let mut changed = original;
        changed.key.invocation = 2;
        alternatives.push(changed);
        for changed in alternatives {
            assert_ne!(expected, request_id(changed).await);
        }
    }

    #[tokio::test]
    async fn missing_mismatched_and_malformed_identities_fail_closed() {
        let original = identity("native-call");
        let key = original.key.clone();
        assert!(channel_request_id(&key).is_err());
        scope(None, async { assert!(channel_request_id(&key).is_err()) }).await;
        scope(Some(original.clone()), async {
            for changed in [
                InvocationKey {
                    run_id: "other".into(),
                    ..key.clone()
                },
                InvocationKey {
                    graph_digest: "other".into(),
                    ..key.clone()
                },
                InvocationKey {
                    node_id: "other".into(),
                    ..key.clone()
                },
                InvocationKey {
                    invocation: 2,
                    ..key.clone()
                },
            ] {
                assert!(channel_request_id(&changed).is_err());
            }
        })
        .await;
        for field in ["", "  ", "call\n", "call\0", "call\u{85}"] {
            let mut changed = original.clone();
            changed.session = field.into();
            scope(Some(changed), async {
                assert!(channel_request_id(&key).is_err())
            })
            .await;
            let mut changed = original.clone();
            changed.tool_call = field.into();
            scope(Some(changed), async {
                assert!(channel_request_id(&key).is_err())
            })
            .await;
        }
        let mut changed = original.clone();
        changed.session = "s".repeat(129);
        scope(Some(changed), async {
            assert!(channel_request_id(&key).is_err())
        })
        .await;
        let mut changed = original.clone();
        changed.tool_call = "c".repeat(513);
        scope(Some(changed), async {
            assert!(channel_request_id(&key).is_err())
        })
        .await;
        for invalid_key in [
            InvocationKey {
                run_id: "".into(),
                ..key.clone()
            },
            InvocationKey {
                graph_digest: "digest\n".into(),
                ..key.clone()
            },
            InvocationKey {
                node_id: "n".repeat(513),
                ..key.clone()
            },
            InvocationKey {
                invocation: 0,
                ..key.clone()
            },
        ] {
            let changed = GooseToolIdentity {
                key: invalid_key.clone(),
                ..original.clone()
            };
            scope(Some(changed), async {
                assert!(channel_request_id(&invalid_key).is_err())
            })
            .await;
        }
    }

    #[tokio::test]
    async fn concurrent_nested_and_spawned_tasks_do_not_leak_identity() {
        let first = identity("first");
        let second = identity("second");
        let key = first.key.clone();
        let (first_id, second_id) =
            tokio::join!(request_id(first.clone()), request_id(second.clone()));
        assert_ne!(first_id, second_id);
        scope(Some(first), async {
            assert_eq!(channel_request_id(&key).unwrap(), first_id);
            scope(Some(second), async {
                tokio::task::yield_now().await;
                assert_eq!(channel_request_id(&key).unwrap(), second_id);
            })
            .await;
            scope(None, async { assert!(channel_request_id(&key).is_err()) }).await;
            let spawned_key = key.clone();
            assert!(
                tokio::spawn(async move { channel_request_id(&spawned_key) })
                    .await
                    .unwrap()
                    .is_err()
            );
            assert_eq!(channel_request_id(&key).unwrap(), first_id);
        })
        .await;
        assert!(channel_request_id(&key).is_err());
    }

    #[tokio::test]
    async fn serialized_binding_has_no_delimiter_ambiguity() {
        let mut first = identity("call");
        first.key.run_id = "run:graph".into();
        first.key.graph_digest = "digest".into();
        let mut second = first.clone();
        second.key.run_id = "run".into();
        second.key.graph_digest = "graph:digest".into();
        assert_eq!(first.key.durable_key(), second.key.durable_key());
        assert_ne!(request_id(first).await, request_id(second).await);
    }
}
