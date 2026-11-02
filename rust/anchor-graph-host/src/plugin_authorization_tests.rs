use super::*;

#[test]
fn authorization_tracks_host_grants_without_freezing_business_code() {
    let declaration = |config| {
        vec![McpServerDefinition {
            name: "api".into(),
            config,
        }]
    };
    let original = declaration(
        serde_json::json!({"command":"${PLUGIN_ROOT}/bin/v1", "args":["old.json"], "cwd":"old", "env":{"KEY":"${TOKEN_A}"}}),
    );
    let updated = declaration(
        serde_json::json!({"command":"${PLUGIN_ROOT}/bin/v2", "args":["new.json"], "cwd":"new", "env":{"KEY":"${TOKEN_A}"}}),
    );
    let authority = plugin_authorization_digest(&original, false, &["data/".into()]);
    assert_eq!(
        authority,
        plugin_authorization_digest(&updated, false, &["data/".into()])
    );
    let other_secret = declaration(
        serde_json::json!({"command":"${PLUGIN_ROOT}/bin/v1", "env":{"KEY":"${TOKEN_B}"}}),
    );
    assert_ne!(
        authority,
        plugin_authorization_digest(&other_secret, false, &["data/".into()])
    );
    assert_ne!(
        authority,
        plugin_authorization_digest(&original, true, &["data/".into()])
    );
    assert_ne!(
        authority,
        plugin_authorization_digest(&original, false, &["other/".into()])
    );
    let host_command =
        declaration(serde_json::json!({"command":"/usr/bin/sh", "env":{"KEY":"${TOKEN_A}"}}));
    assert_ne!(
        authority,
        plugin_authorization_digest(&host_command, false, &["data/".into()])
    );
}

#[test]
fn remote_authority_keeps_credential_selectors_but_ignores_literal_rotation() {
    let server = |header| {
        vec![McpServerDefinition {
            name: "api".into(),
            config: serde_json::json!({"url":"https://example.test/mcp", "headers":{"Authorization":header}}),
        }]
    };
    assert_eq!(
        plugin_authorization_digest(&server("Bearer literal-a"), true, &[]),
        plugin_authorization_digest(&server("Bearer literal-b"), true, &[])
    );
    assert_ne!(
        plugin_authorization_digest(&server("Bearer ${TOKEN_A}"), true, &[]),
        plugin_authorization_digest(&server("Bearer ${TOKEN_B}"), true, &[])
    );
    let mut scope = server("Bearer ${TOKEN_A}");
    let before = plugin_authorization_digest(&scope, true, &[]);
    scope[0].config["oauth_scope"] = serde_json::json!("write");
    assert_ne!(before, plugin_authorization_digest(&scope, true, &[]));
}

#[test]
fn channel_identity_disclosure_is_part_of_the_authority_only_when_declared() {
    let server = |config| {
        vec![McpServerDefinition {
            name: "api".into(),
            config,
        }]
    };
    let plain =
        serde_json::json!({"url":"https://example.test/mcp","bearer_token_env_var":"TOKEN"});
    let mut disclosed = plain.clone();
    disclosed["channel_identity_headers"] = serde_json::json!({"X-Sender":"sender_id"});
    let mut other = plain.clone();
    other["channel_identity_headers"] = serde_json::json!({"X-Sender":"conversation_id"});
    let base = plugin_authorization_digest(&server(plain), true, &[]);
    let granted = plugin_authorization_digest(&server(disclosed), true, &[]);
    assert_ne!(base, granted);
    assert_ne!(
        granted,
        plugin_authorization_digest(&server(other), true, &[])
    );
}
