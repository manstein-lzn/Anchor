use super::*;
use crate::plugin_library::PluginLibrary;
use anchor_graph_host::{PluginCatalog, plugin_view_path, resolved_library_root, store_root};
use anchor_library::OAuthAuthorizationMetadata;
use anchor_sandbox_bwrap::BubblewrapPolicy;
use axum::{Json, Router, routing::post};
use std::{fs, os::unix::fs::PermissionsExt, sync::atomic::AtomicBool, time::Duration};

const PLUGIN: &str = "changing";

#[tokio::test]
async fn live_default_preparation_preserves_canonical_library_and_plugin_symlink_reads() {
    let root = tempfile::tempdir().unwrap();
    let library_root = root.path().join("real-library");
    let alias = root.path().join("library-alias");
    let source = root.path().join("external-source");
    fs::create_dir_all(library_root.join("plugins")).unwrap();
    fs::create_dir_all(source.join("skills/probe")).unwrap();
    fs::create_dir_all(source.join("data")).unwrap();
    fs::write(
        source.join("plugin.json"),
        r#"{"name":"Linked Skill","skills":"skills/","mutable":["data/"]}"#,
    )
    .unwrap();
    fs::write(source.join("skills/probe/SKILL.md"), "canonical skill").unwrap();
    fs::write(source.join("data/live.txt"), "live business bytes").unwrap();
    std::os::unix::fs::symlink(&library_root, &alias).unwrap();
    std::os::unix::fs::symlink(&source, library_root.join("plugins").join(PLUGIN)).unwrap();
    let definition = Catalog::new(&alias).definition(PLUGIN).unwrap();
    let prepared = prepare(&alias, definition.binding()).await;
    assert_eq!(
        prepared.mounts,
        [ReadOnlyInput::new(&source, "/plugins/changing")]
    );
    let mut request = SandboxRequest::new(root.path().join("workspace"), vec!["sh"]);
    prepared.views[PLUGIN].apply(&mut request);
    assert_eq!(request.writable_inputs[0].source, source.join("data"));
    assert_eq!(
        prepared
            .disclosure
            .read(&json!({"plugin":PLUGIN,"section":"skill","path":"skills/probe/SKILL.md"}))
            .unwrap()["text"],
        "canonical skill"
    );
    assert_eq!(
        fs::read_to_string(source.join("data/live.txt")).unwrap(),
        "live business bytes"
    );
    // The live-read capability does not relax any Library write boundary.
    let aliased = anchor_library::Library::new(&alias);
    assert!(matches!(
        aliased.try_install_lease(),
        Err(anchor_library::InstallError::UnsafePath)
    ));
    drop(prepared);
    let managed = anchor_library::Library::new(&library_root);
    let lease = managed.install_lease().unwrap();
    assert!(matches!(
        managed.retain_revision_locked(PLUGIN, &source, &definition.digest, None, &lease),
        Err(anchor_library::RevisionError::Install(
            anchor_library::InstallError::UnsafePath
        ))
    ));
}

#[tokio::test]
async fn a_pending_default_publication_is_a_known_outage_but_exact_skill_sources_still_prepare() {
    let root = tempfile::tempdir().unwrap();
    let library_root = root.path().join("library");
    let source = root.path().join("source");
    fs::create_dir_all(source.join("skills/probe")).unwrap();
    fs::write(
        source.join("plugin.json"),
        r#"{"name":"Skill","skills":"skills/"}"#,
    )
    .unwrap();
    fs::write(source.join("skills/probe/SKILL.md"), "retained skill").unwrap();
    let library = anchor_library::Library::new(&library_root);
    let installed = library.install_directory(PLUGIN, &source, false).unwrap();
    let binding = Catalog::new(&library_root)
        .definition(PLUGIN)
        .unwrap()
        .binding();
    let lease = library.install_lease().unwrap();
    library
        .retain_revision_locked(PLUGIN, &source, &installed.digest, None, &lease)
        .unwrap();
    library
        .set_publication_fence_locked(
            PLUGIN,
            &anchor_library::PluginPublicationFence {
                execute_digest: "a".repeat(64),
                intent_digest: "b".repeat(64),
            },
            &lease,
        )
        .unwrap();
    drop(lease);
    let default = crate::plugin_library::tools::prepare_plugins_async(
        vec![binding.clone()],
        Some(library_root.clone()),
        PluginToolHost::new(Vec::<String>::new()),
        Arc::new(AtomicBool::new(false)),
    )
    .await;
    assert!(
        matches!(default, Err(crate::plugin_view::Unusable::KnownFailure(ref reason)) if reason == "plugin_default_publication_pending")
    );
    let pinned = crate::plugin_library::tools::prepare_plugins_for_owner_with_revisions_async(
        vec![binding],
        Some(library_root),
        PluginToolHost::new(Vec::<String>::new()),
        Arc::new(AtomicBool::new(false)),
        None,
        BTreeMap::from([(PLUGIN.to_owned(), installed.digest)]),
    )
    .await
    .unwrap();
    assert_eq!(
        pinned
            .disclosure
            .read(&json!({"plugin":PLUGIN,"section":"skill","path":"skills/probe/SKILL.md"}))
            .unwrap()["text"],
        "retained skill"
    );
}

fn generation(directory: &Path, revision: &str, dependency: &str, remote: Option<&str>) {
    fs::create_dir_all(directory.join("bin")).unwrap();
    fs::create_dir_all(directory.join("skills").join(revision)).unwrap();
    let entrypoint = directory.join("bin").join(revision);
    fs::write(&entrypoint, "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&entrypoint, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(
        directory.join("skills").join(revision).join("SKILL.md"),
        revision,
    )
    .unwrap();
    let mut manifest = json!({
        "name": "Changing", "skills": "skills/",
        "dependencies": {"python": [format!("idna=={dependency}")]},
        "mcpServers": {"local": {
            "command": format!("${{PLUGIN_ROOT}}/bin/{revision}"),
            "args": [format!("argument-{revision}")]
        }}
    });
    if let Some(remote) = remote {
        manifest["mcpServers"]["remote"] = json!({
            "url": remote, "oauth_resource": remote, "auth": "oauth"
        });
    }
    fs::write(
        directory.join("plugin.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
}

fn seed_view(root: &Path, definition: &PluginDefinition, dependency: &str) -> PathBuf {
    let root = resolved_library_root(root);
    let view = plugin_view_path(
        &root,
        &definition.id,
        &definition.dependencies.declaration_digest(),
    );
    let package = store_root(&root).join("idna").join(dependency).join("idna");
    let site = view.join("lib/python3.12/site-packages");
    fs::create_dir_all(&package).unwrap();
    fs::create_dir_all(&site).unwrap();
    fs::create_dir_all(view.join("bin")).unwrap();
    fs::write(
        view.join(crate::plugin_view::VIEW_CONFIGURATION),
        "home = /usr/bin\n",
    )
    .unwrap();
    std::os::unix::fs::symlink("/bin/sh", view.join("bin/python")).unwrap();
    std::os::unix::fs::symlink(package, site.join("idna")).unwrap();
    view
}

async fn prepare(
    root: &Path,
    binding: PluginBinding,
) -> crate::plugin_library::tools::PreparedPlugins {
    crate::plugin_library::tools::prepare_plugins_async(
        vec![binding],
        Some(root.to_path_buf()),
        PluginToolHost::new(Vec::<String>::new()),
        Arc::new(AtomicBool::new(false)),
    )
    .await
    .unwrap()
}

fn assert_generation(
    prepared: &crate::plugin_library::tools::PreparedPlugins,
    definition: &PluginDefinition,
    revision: &str,
) {
    assert_eq!(prepared.bindings, [definition.binding()]);
    assert_eq!(
        prepared.mcp.servers["changing-local"].config["command"],
        definition
            .directory
            .join("bin")
            .join(revision)
            .to_string_lossy()
            .as_ref()
    );
    assert_eq!(
        prepared.mcp.servers["changing-local"].config["args"],
        json!([format!("argument-{revision}")])
    );
    assert_eq!(
        prepared.mcp.servers["changing-local"].plugin_directory,
        definition.directory
    );
    assert_eq!(
        prepared.mounts,
        [ReadOnlyInput::new(
            &definition.directory,
            "/plugins/changing"
        )]
    );
    assert_eq!(
        prepared.views[PLUGIN].declaration_digest(),
        Some(definition.dependencies.declaration_digest().as_str())
    );
    assert!(
        prepared.bindings[0]
            .resources
            .contains(&format!("skills/{revision}/SKILL.md"))
    );
}

#[tokio::test]
async fn a_real_install_during_publication_wait_prepares_one_current_definition() {
    let _environment = crate::PROCESS_ENV_LOCK.lock().await;
    let root = tempfile::tempdir().unwrap();
    let library_root = root.path().join("library");
    let first_source = root.path().join("first");
    let next_source = root.path().join("next");
    generation(&first_source, "first", "3.6", None);
    generation(&next_source, "next", "3.7", None);
    let library = PluginLibrary::new(&library_root);
    library
        .install_directory(PLUGIN, &first_source, false)
        .unwrap();
    let catalog = Catalog::new(&library_root);
    let first = catalog.definition(PLUGIN).unwrap();
    seed_view(&library_root, &first, "3.6");

    let lease = library.install_lease().unwrap();
    let prepare_root = library_root.clone();
    let first_binding = first.binding();
    let mut preparing = tokio::spawn(async move { prepare(&prepare_root, first_binding).await });
    assert!(
        tokio::time::timeout(Duration::from_millis(40), &mut preparing)
            .await
            .is_err()
    );
    library
        .install_directory_locked(PLUGIN, &next_source, true, &lease)
        .unwrap();
    let next = catalog.definition(PLUGIN).unwrap();
    let view = seed_view(&library_root, &next, "3.7");
    assert_ne!(first.digest, next.digest);
    assert_ne!(first.resources, next.resources);
    assert!(first.binding().same_authorization(&next.binding()));
    drop(lease);

    let prepared = tokio::time::timeout(Duration::from_secs(5), preparing)
        .await
        .unwrap()
        .unwrap();
    assert_generation(&prepared, &next, "next");
    assert_eq!(prepared.views[PLUGIN].view(), Some(view.as_path()));
}

struct NeverRefresh;
impl OAuthRefreshTransport for NeverRefresh {
    fn refresh(
        &self,
        _: &OAuthRefreshRequest<'_>,
    ) -> Result<OAuthTokenResponse, OAuthTransportError> {
        Err(OAuthTransportError::RequestFailed)
    }
}

#[tokio::test]
async fn oauth_releases_publication_lease_and_cannot_mix_a_reclaimed_view_with_new_config() {
    let _environment = crate::PROCESS_ENV_LOCK.lock().await;
    let root = tempfile::tempdir().unwrap();
    let library_root = root.path().join("library");
    let entered = Arc::new(tokio::sync::Notify::new());
    let resume = Arc::new(tokio::sync::Notify::new());
    let notify_entered = entered.clone();
    let wait_resume = resume.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let resource = format!("http://{address}/mcp");
    let provider = Router::new().route("/token", post(move || {
        let entered = notify_entered.clone();
        let resume = wait_resume.clone();
        async move {
            entered.notify_one();
            resume.notified().await;
            Json(json!({"access_token":"refreshed-secret","token_type":"Bearer","expires_in":3600}))
        }
    }));
    let provider_task = tokio::spawn(async move { axum::serve(listener, provider).await.unwrap() });
    let first_source = root.path().join("first");
    let next_source = root.path().join("next");
    generation(&first_source, "first", "3.6", Some(&resource));
    generation(&next_source, "next", "3.7", Some(&resource));
    let library = PluginLibrary::new(&library_root);
    library
        .install_directory(PLUGIN, &first_source, false)
        .unwrap();
    let catalog = Catalog::new(&library_root);
    let first = catalog.definition(PLUGIN).unwrap();
    let old_view = seed_view(&library_root, &first, "3.6");
    let owner = "preparation-owner";
    let oauth = OAuthBinding::new(
        format!("{:x}", Sha256::digest(resource.as_bytes())),
        PLUGIN,
        "remote",
        owner,
    )
    .unwrap();
    OAuthClient::new(FileOAuthTokenStore::new(&library_root), NeverRefresh)
        .authorize(
            oauth,
            OAuthAuthorizationMetadata::new(format!("http://{address}/token"))
                .unwrap()
                .with_client_id("fixture")
                .unwrap()
                .with_resource(resource)
                .unwrap(),
            OAuthTokenResponse::new("expired", Some("refresh".into()), Some(1)).unwrap(),
            0,
        )
        .unwrap();
    let mut prepared = prepare(&library_root, first.binding()).await;
    // Use this fixture's explicit Library regardless of the deployment settings.
    prepared.mcp.oauth_library_root = Some(library_root.clone());
    let authorizing = tokio::spawn(async move {
        prepared
            .mcp
            .authorize_oauth(Some(owner.into()), true)
            .await
            .unwrap();
        prepared
    });
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .unwrap();
    let lease = library
        .try_install_lease()
        .expect("OAuth must not retain the publication lease");
    library
        .install_directory_locked(PLUGIN, &next_source, true, &lease)
        .unwrap();
    let next = catalog.definition(PLUGIN).unwrap();
    seed_view(&library_root, &next, "3.7");
    drop(lease);
    assert!(
        !old_view.exists(),
        "the real installer reclaims the old declaration's view"
    );
    resume.notify_one();
    let prepared = tokio::time::timeout(Duration::from_secs(5), authorizing)
        .await
        .unwrap()
        .unwrap();
    assert_generation(&prepared, &first, "first");
    assert_eq!(
        prepared.mcp.servers["changing-remote"].config["headers"]["Authorization"],
        "Bearer refreshed-secret"
    );

    let workspace = root.path().join("workspace");
    fs::create_dir(&workspace).unwrap();
    let sandbox = BubblewrapSandbox::new(
        BubblewrapPolicy::new("bwrap", ["sh"]).authorize_workspace_root(&workspace),
    )
    .unwrap();
    let result = prepared
        .mcp
        .bind_with_node_inputs(
            &prepared.bindings,
            true,
            SandboxLaunch {
                sandbox: &sandbox,
                workspace: &workspace,
                readonly_inputs: &prepared.mounts,
                plugin_views: &prepared.views,
            },
        )
        .await;
    assert!(
        result.is_err(),
        "a reclaimed prepared view must fail instead of falling back to the new view"
    );
    let current = prepare(&library_root, first.binding()).await;
    assert_generation(&current, &next, "next");
    provider_task.abort();
}

#[test]
fn preliminary_wiring_refuses_legacy_identity_and_validates_skill_authority() {
    let root = tempfile::tempdir().unwrap();
    let plugin = root.path().join("plugins/skill");
    fs::create_dir_all(&plugin).unwrap();
    fs::write(plugin.join("plugin.json"), r#"{"name":"Skill"}"#).unwrap();
    let catalog = Catalog::new(root.path());
    let binding = catalog.resolve(&["skill".into()]).unwrap().remove(0);
    assert!(binding.mcp_servers.is_empty());
    let mut legacy = serde_json::to_value(&binding).unwrap();
    legacy["authorization_digest"] = serde_json::json!("");
    McpToolConfig::from_catalog(root.path(), std::slice::from_ref(&binding)).unwrap();
    let error = serde_json::from_value::<PluginBinding>(legacy.clone())
        .unwrap_err()
        .to_string();
    assert!(error.contains("old authorization format"), "{error}");
    assert!(error.contains("offline migration required"), "{error}");
    let mut forged = binding.clone();
    forged.authorization_digest = "forged".try_into().expect("non-empty fixture authority");
    assert!(McpToolConfig::from_catalog(root.path(), &[forged]).is_err());
    // Declaring a new MCP server changes the authority digest, so a binding this
    // Host read before the edit no longer matches the catalog and assembly stops.
    fs::write(
        plugin.join("plugin.json"),
        r#"{"name":"Skill","mcpServers":{"probe":{"type":"http","url":"http://127.0.0.1:9/mcp"}}}"#,
    )
    .unwrap();
    assert!(McpToolConfig::from_catalog(root.path(), &[binding]).is_err());
    fs::write(plugin.join("instructions.md"), "changed code and resources").unwrap();
    assert!(serde_json::from_value::<PluginBinding>(legacy).is_err());
}

#[tokio::test]
async fn preparation_requires_real_skill_authority_and_deployment_enabled_exact_fixtures() {
    let root = tempfile::tempdir().unwrap();
    let plugin = root.path().join("plugins/skill");
    fs::create_dir_all(&plugin).unwrap();
    fs::write(plugin.join("plugin.json"), r#"{"name":"Skill"}"#).unwrap();
    let catalog = Catalog::new(root.path());
    let mut skill = catalog.resolve(&["skill".into()]).unwrap().remove(0);
    skill.authorization_digest = "forged".try_into().expect("non-empty fixture authority");
    let enabled = PluginToolHost::new(["fake-tools".into()]);
    let cancellation = || Arc::new(AtomicBool::new(false));
    assert!(
        crate::plugin_library::tools::prepare_plugins_async(
            vec![skill.clone()],
            Some(root.path().to_path_buf()),
            enabled.clone(),
            cancellation()
        )
        .await
        .is_err()
    );
    assert!(
        crate::plugin_library::tools::prepare_plugins_async(
            vec![skill],
            None,
            enabled.clone(),
            cancellation()
        )
        .await
        .is_err()
    );
    let mut forged = fake_binding();
    forged.id = "skill".into();
    assert!(McpToolConfig::from_catalog(root.path(), std::slice::from_ref(&forged)).is_err());
    assert!(
        crate::plugin_library::tools::prepare_plugins_async(
            vec![forged],
            Some(root.path().to_path_buf()),
            enabled.clone(),
            cancellation()
        )
        .await
        .is_err()
    );
    assert!(
        crate::plugin_library::tools::prepare_plugins_async(
            vec![fake_binding()],
            None,
            PluginToolHost::new(Vec::<String>::new()),
            cancellation()
        )
        .await
        .is_err()
    );
    let prepared = crate::plugin_library::tools::prepare_plugins_async(
        vec![fake_binding()],
        None,
        enabled,
        cancellation(),
    )
    .await
    .unwrap();
    assert_eq!(prepared.bindings, [fake_binding()]);
    assert!(prepared.mounts.is_empty());
}

#[tokio::test]
async fn exact_revision_prepares_old_code_mcp_and_solve_with_current_mutable_authority() {
    let root = tempfile::tempdir().unwrap();
    let library_root = root.path().join("library");
    let first_source = root.path().join("first");
    let next_source = root.path().join("next");
    generation(&first_source, "first", "3.6", None);
    generation(&next_source, "next", "3.7", None);
    for source in [&first_source, &next_source] {
        let path = source.join("plugin.json");
        let mut manifest: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        manifest["mutable"] = json!(["data/"]);
        fs::write(path, manifest.to_string()).unwrap();
    }
    let library = anchor_library::Library::new(&library_root);
    library
        .install_directory(PLUGIN, &first_source, false)
        .unwrap();
    let data = library_root.join("plugins/changing/data");
    fs::write(data.join("counter"), "42").unwrap();
    let first = Catalog::new(&library_root).definition(PLUGIN).unwrap();
    let declaration_view = seed_view(&library_root, &first, "3.6");
    // A real, hashed fixture executable supplies identity only. This test does
    // not claim Python package execution; the provisioning tests cover that.
    let interpreter = fs::canonicalize("/bin/sh").unwrap();
    let lock = format!("idna==3.6 --hash=sha256:{}\n", "a".repeat(64));
    let identity = anchor_library::DependencyViewIdentity {
        declaration_digest: first.dependencies.declaration_digest(),
        lock_digest: format!("{:x}", Sha256::digest(lock.as_bytes())),
        lock,
        interpreter: "fixture".into(),
        interpreter_prefix: interpreter.parent().unwrap().parent().unwrap().to_owned(),
        interpreter_digest: format!("{:x}", Sha256::digest(fs::read(&interpreter).unwrap())),
        interpreter_path: interpreter,
    };
    let actual_view = identity.view_path(&library_root, PLUGIN);
    fs::rename(&declaration_view, &actual_view).unwrap();
    let lease = library.install_lease().unwrap();
    let revision = library
        .retain_revision_locked(PLUGIN, &first_source, &first.digest, Some(identity), &lease)
        .unwrap();
    library
        .install_directory_locked(PLUGIN, &next_source, true, &lease)
        .unwrap();
    let next = Catalog::new(&library_root).definition(PLUGIN).unwrap();
    seed_view(&library_root, &next, "3.7");
    drop(lease);
    let exact = library.resolve_source(PLUGIN, Some(&first.digest)).unwrap();
    let exact_definition = exact.definition().clone();
    drop(exact);
    let prepared = crate::plugin_library::tools::prepare_plugins_for_owner_with_revisions_async(
        vec![first.binding()],
        Some(library_root.clone()),
        PluginToolHost::new(Vec::<String>::new()),
        Arc::new(AtomicBool::new(false)),
        None,
        BTreeMap::from([(PLUGIN.to_owned(), first.digest.clone())]),
    )
    .await
    .unwrap();
    assert_generation(&prepared, &exact_definition, "first");
    assert_eq!(prepared.views[PLUGIN].view(), Some(actual_view.as_path()));
    assert_eq!(
        prepared
            .disclosure
            .read(&json!({
                "plugin":PLUGIN,"section":"skill","path":"skills/first/SKILL.md"
            }))
            .unwrap()["text"],
        "first"
    );
    let mut request = SandboxRequest::new(root.path().join("workspace"), vec!["sh"]);
    prepared.views[PLUGIN].apply(&mut request);
    assert_eq!(request.writable_inputs[0].source, data);
    assert_eq!(
        request.writable_inputs[0].destination,
        Path::new("/plugins/changing/data")
    );
    assert_eq!(fs::read_to_string(data.join("counter")).unwrap(), "42");
    assert_eq!(
        fs::read_dir(exact_definition.directory.join("data"))
            .unwrap()
            .count(),
        0
    );
    assert!(
        actual_view.is_dir(),
        "real default installation must retain the exact solve"
    );
    let current = prepare(&library_root, next.binding()).await;
    assert_generation(&current, &next, "next");
    let lease = library
        .try_install_lease()
        .expect("pins must not hold the global catalog lease");
    assert!(matches!(
        library.release_revision_locked(&revision, &BTreeSet::new(), &lease),
        Err(anchor_library::RevisionError::InUse)
    ));
    drop(current);
    drop(prepared);
    assert!(
        library
            .release_revision_locked(&revision, &BTreeSet::new(), &lease)
            .unwrap()
    );
}
