//! Credential bytes remain opaque while declaration-owned mount paths expand.
use super::*;
use std::fs;

#[test]
fn injected_stdio_credentials_are_not_view_or_plugin_path_templates() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().canonicalize().unwrap();
    let secret = format!(
        "opaque-密钥-${{ANCHOR_PLUGIN_VIEW}}-{}/token",
        directory.display()
    );
    let declaration = json!({
        "command":"${ANCHOR_PLUGIN_VIEW}/bin/python3",
        "args":["${PLUGIN_ROOT}/server.py", "--root=${PLUGIN_ROOT}/assets", "--opaque=${ALIAS}"],
        "env_vars":["REQUIRED_TOKEN"], "optional_env_vars":["OPTIONAL_TOKEN"],
        "env":{
            "REQUIRED_TOKEN":"${ANCHOR_PLUGIN_VIEW}/overridden-by-injection",
            "PATHS":"${PLUGIN_ROOT}/bin:${ANCHOR_PLUGIN_VIEW}/bin",
            "ALIAS":"${ALIAS}", "LITERAL":"literal"
        }
    });
    let resolved = json!({
        "command":"${ANCHOR_PLUGIN_VIEW}/bin/python3",
        "args":[directory.join("server.py"), format!("--root={}/assets", directory.display()), format!("--opaque={secret}")],
        "env_vars":["REQUIRED_TOKEN"], "optional_env_vars":["OPTIONAL_TOKEN"],
        "env":{
            "REQUIRED_TOKEN":secret, "OPTIONAL_TOKEN":secret,
            "PATHS":format!("{}/bin:${{ANCHOR_PLUGIN_VIEW}}/bin", directory.display()),
            "ALIAS":secret, "LITERAL":format!("literal-{}", directory.display())
        }
    });
    let named = named_view(
        &resolved,
        &declaration,
        Some(Path::new("/environments/view")),
        "demo",
    )
    .unwrap();
    let (program, args, environment, _) =
        stdio_config(&named, &declaration, &directory, "demo").unwrap();
    assert_eq!(program, "/environments/view/bin/python3");
    assert_eq!(
        args,
        vec![
            "/plugins/demo/server.py".to_owned(),
            "--root=/plugins/demo/assets".to_owned(),
            format!("--opaque={secret}")
        ]
    );
    for key in ["REQUIRED_TOKEN", "OPTIONAL_TOKEN", "ALIAS"] {
        assert_eq!(
            environment
                .iter()
                .find(|value| value.key == key)
                .unwrap()
                .value()
                .as_bytes(),
            secret.as_bytes()
        );
    }
    assert_eq!(
        environment
            .iter()
            .find(|value| value.key == "PATHS")
            .unwrap()
            .value(),
        "/plugins/demo/bin:/environments/view/bin"
    );
    assert_eq!(
        environment
            .iter()
            .find(|value| value.key == "LITERAL")
            .unwrap()
            .value(),
        format!("literal-{}", directory.display())
    );
}

#[test]
fn only_a_declared_view_template_requires_a_view_grant() {
    let secret = "opaque-${ANCHOR_PLUGIN_VIEW}-credential";
    let declaration = json!({"command":"echo", "env_vars":["TOKEN"], "env":{"TOKEN":"${ANCHOR_PLUGIN_VIEW}","ALIAS":"${OPAQUE}"}});
    let resolved =
        json!({"command":"echo", "env_vars":["TOKEN"], "env":{"TOKEN":secret,"ALIAS":secret}});
    assert_eq!(
        named_view(&resolved, &declaration, None, "demo").unwrap(),
        resolved
    );
    let declaration = json!({"command":"${ANCHOR_PLUGIN_VIEW}/bin/python3"});
    let error = named_view(&resolved, &declaration, None, "demo").unwrap_err();
    assert!(error.contains("no usable dependency view"));
    assert!(!error.contains(secret));
}

#[test]
fn owner_snapshot_preserves_shared_http_and_stdio_credential_bytes() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("plugins/demo");
    let data = root.path().join("state");
    fs::create_dir_all(&directory).unwrap();
    fs::create_dir_all(data.join("platform")).unwrap();
    fs::write(directory.join("plugin.json"), json!({
        "name":"Opaque credentials", "configuration":{
            "fields":{"key":{"type":"secret","label":"Key"}},
            "environment":{"remote":{"BEARER_KEY":"key","HEADER_KEY":"key"},
                "local":{"REQUIRED_KEY":"key","OPTIONAL_KEY":"key"}}
        },
        "mcpServers":{
            "remote":{"url":"https://example.test/mcp", "bearer_token_env_var":"BEARER_KEY", "env_http_headers":{"X-Key":"HEADER_KEY"}},
            "local":{"command":"echo", "env_vars":["REQUIRED_KEY"], "optional_env_vars":["OPTIONAL_KEY"], "env":{"ROOT":"${PLUGIN_ROOT}/data"}}
        }
    }).to_string()).unwrap();
    let definition = Catalog::new(root.path()).definition("demo").unwrap();
    let secret = format!(
        "opaque-${{ANCHOR_PLUGIN_VIEW}}-{}/token",
        definition.directory.display()
    );
    let sessions =
        anchor_platform_session::SessionStore::open(data.join("platform/sessions.sqlite")).unwrap();
    let update = serde_json::from_value(
        json!({"authorization_digest":definition.authorization_digest,"revision":0,
        "fields":{"key":{"action":"set","value":secret}}}),
    )
    .unwrap();
    crate::plugin_configuration::configure(&data, root.path(), "owner", "demo", update, &sessions)
        .unwrap();
    let owner = crate::credential_store::binding_owner("owner");
    let config = McpToolConfig::from_definitions_for_owner(
        root.path(),
        &[definition.binding()],
        &BTreeMap::from([("demo".into(), definition.clone())]),
        Some((&data, &owner)),
    )
    .unwrap();
    assert_eq!(config.configuration_revisions["demo"], 1);
    assert!(config.configuration_bindings.contains_key("demo"));
    assert_eq!(config.credential_process_plugins(), vec!["demo"]);
    let local = &config.servers["demo-local"];
    let named = named_view(&local.config, &local.declaration, None, "demo").unwrap();
    let (_, _, environment, _) =
        stdio_config(&named, &local.declaration, &local.plugin_directory, "demo").unwrap();
    for key in ["REQUIRED_KEY", "OPTIONAL_KEY"] {
        assert_eq!(
            environment
                .iter()
                .find(|value| value.key == key)
                .unwrap()
                .value()
                .as_bytes(),
            secret.as_bytes()
        );
    }
    assert_eq!(
        environment
            .iter()
            .find(|value| value.key == "ROOT")
            .unwrap()
            .value(),
        "/plugins/demo/data"
    );
    let remote = &config.servers["demo-remote"];
    let named = named_view(&remote.config, &remote.declaration, None, "demo").unwrap();
    assert_eq!(
        named["headers"]["Authorization"]
            .as_str()
            .unwrap()
            .as_bytes(),
        format!("Bearer {secret}").as_bytes()
    );
    let (_, bearer, headers) = http_config(&named).unwrap();
    assert!(bearer.is_some());
    assert_eq!(headers["X-Key"].as_bytes(), secret.as_bytes());
}
