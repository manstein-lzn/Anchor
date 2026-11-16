use super::*;
use crate::node_host::mounts::MOUNT_EVIDENCE_KEY;

#[test]
fn plugin_free_schedule_mounts_ignore_installation_but_session_mounts_remain_guarded() {
    let (_root, resolver, _mounts, mut request) = session_fixture(&[], &[], &[]);
    request.kind = NodeKind::OpRun;
    let state = resolver.local_inputs.state_root();
    let mut metadata = crate::run_metadata::load(state, &request.key.run_id)
        .unwrap()
        .unwrap();
    let pilot = metadata.pilot.take();
    let trigger: crate::run_metadata::RunTrigger = serde_json::from_value(json!({
        "source":"schedule", "schedule":"plan", "scheduled_at":"2026-11-11T09:00:00"
    }))
    .unwrap();
    trigger.apply(&mut metadata);
    crate::run_metadata::save(state, &metadata).unwrap();
    let lease = crate::plugin_library::PluginLibrary::new(resolver.plugin_library_root().unwrap())
        .install_lease()
        .unwrap();

    let mounted = resolver.mounted_plugins(&request).unwrap();
    assert!(mounted.bindings.is_empty());
    assert!(!mounted.network);
    assert!(mounted.session.is_none());

    // Empty Graph declarations do not prove an empty Session policy. Keep its
    // frozen switches behind the catalog guard, even when they are empty now.
    metadata.pilot = pilot;
    crate::run_metadata::save(state, &metadata).unwrap();
    let error = resolver.mounted_plugins(&request).unwrap_err();
    assert!(
        error.contains("Plugin catalog is being installed"),
        "{error}"
    );
    drop(lease);
    let mounted = resolver.mounted_plugins(&request).unwrap();
    assert_eq!(mounted.session.as_deref(), Some("session-one"));
    assert!(!mounted.network);
}

#[test]
fn network_enabled_node_still_observes_catalog_installation() {
    let snapshot = anchor_runtime::graph::GraphSnapshot::admit(json!({
        "objective":"network mount","entry":"agent",
        "agents":{"worker":{"model":"fixture"}},
        "nodes":[{"id":"agent","agent":"worker","plugins":[{
            "id":"network","required":true,"network":"enabled"
        }]}],"edges":[]
    }))
    .unwrap();
    let (_root, resolver, _mounts, mut request) = session_fixture_with_snapshot(
        &["network"],
        &[("network", true, None)],
        &[],
        Some(snapshot),
    );
    assert!(request.plugins.iter().any(|plugin| plugin.id == "network"));
    // The admitted policy still needs the catalog when a caller omits bindings.
    request.plugins.clear();
    let state = resolver.local_inputs.state_root();
    let mut metadata = crate::run_metadata::load(state, &request.key.run_id)
        .unwrap()
        .unwrap();
    metadata.pilot = None;
    crate::run_metadata::save(state, &metadata).unwrap();
    let _lease = crate::plugin_library::PluginLibrary::new(resolver.plugin_library_root().unwrap())
        .install_lease()
        .unwrap();

    let error = resolver.mounted_plugins(&request).unwrap_err();
    assert!(
        error.contains("Plugin catalog is being installed"),
        "{error}"
    );
}

#[test]
fn a_plugin_authorization_change_still_fails_closed() {
    let (_root, resolver, _mounts, mut request) =
        session_fixture(&["review"], &[("review", true, None)], &[]);
    let library = resolver.plugin_library_root().unwrap();
    let admitted = crate::plugin_library::Catalog::new(library)
        .resolve(&["review".into()])
        .unwrap()
        .remove(0);
    seed_admitted_binding(&resolver, &mut request, admitted);
    std::fs::write(
        library.join("plugins/review/plugin.json"),
        r#"{"name":"review","network":true}"#,
    )
    .unwrap();

    let error = resolver.mounted_plugins(&request).unwrap_err();
    assert!(error.contains("Host authorization changed"), "{error}");
}

#[test]
fn a_session_mounted_plugin_the_graph_never_declared_is_mounted() {
    let (_root, resolver, _mounts, request) = session_fixture(&["side"], &[], &[("side", true)]);
    let mounted = resolver.mounted_plugins(&request).unwrap();
    assert_eq!(mounted_ids(&mounted), ["side"]);
    // The human's click authorized it, not the Graph: the binding came from
    // the Library, with the identity the catalog resolved.
    assert!(!mounted.bindings[0].digest.is_empty());
    assert_eq!(mounted.session.as_deref(), Some("session-one"));
    assert!(mounted.snapshot, "the Turn's switches are pinned");
}

#[tokio::test]
async fn a_session_skill_returns_its_current_resources_with_the_frozen_tools() {
    let (_root, resolver, _mounts, request) = session_fixture(&["side"], &[], &[("side", true)]);
    let plugin = resolver.plugin_library_root().unwrap().join("plugins/side");
    std::fs::create_dir_all(plugin.join("skills/check")).unwrap();
    std::fs::write(
        plugin.join("skills/check/SKILL.md"),
        "read the current Skill",
    )
    .unwrap();
    assert!(request.plugins.is_empty());
    let resolved = resolver.tools(&request).await.unwrap();
    assert_eq!(resolved.plugins.len(), 1);
    assert_eq!(resolved.plugins[0].id, "side");
    assert!(
        resolved.plugins[0]
            .resources
            .contains(&"skills/check/SKILL.md".to_owned())
    );
    assert_eq!(
        resolver.observed_plugin_digests(&request.key),
        BTreeMap::from([("side".to_owned(), resolved.plugins[0].digest.clone())])
    );
}

#[test]
fn a_session_switched_off_plugin_is_not_mounted() {
    let (_root, resolver, _mounts, request) = session_fixture(
        &["side", "kept"],
        &[("side", false, None), ("kept", true, None)],
        &[("side", false)],
    );
    let mounted = resolver.mounted_plugins(&request).unwrap();
    // An optional Plugin the Session switched off stays off; a required one
    // is mounted regardless of the switch.
    assert_eq!(mounted_ids(&mounted), ["kept"]);
    assert!(
        mounted
            .reasons
            .iter()
            .any(|reason| reason.contains("`side`") && reason.contains("switched off")),
        "{:?}",
        mounted.reasons
    );
}

/// An optional Plugin follows "the Session's explicit value, else its
/// `default`, else on" -- and a required one ignores the Session entirely.
#[test]
fn an_optional_default_decides_until_the_session_says_otherwise() {
    // `default: true` and no Session value: mounted.
    let (_root, resolver, _mounts, request) =
        session_fixture(&["opt-on"], &[("opt-on", false, Some(true))], &[]);
    assert_eq!(
        mounted_ids(&resolver.mounted_plugins(&request).unwrap()),
        ["opt-on"]
    );

    // `default: false` and no Session value: not mounted.
    let (_root, resolver, _mounts, request) =
        session_fixture(&["opt-off"], &[("opt-off", false, Some(false))], &[]);
    let mounted = resolver.mounted_plugins(&request).unwrap();
    assert!(mounted_ids(&mounted).is_empty(), "{:?}", mounted.reasons);
    assert!(
        mounted
            .reasons
            .iter()
            .any(|reason| reason.contains("opt-off")),
        "{:?}",
        mounted.reasons
    );

    // The Session's explicit value beats a `default: false`.
    let (_root, resolver, _mounts, request) = session_fixture(
        &["opt-off"],
        &[("opt-off", false, Some(false))],
        &[("opt-off", true)],
    );
    assert_eq!(
        mounted_ids(&resolver.mounted_plugins(&request).unwrap()),
        ["opt-off"]
    );

    // ...and `default: true` does not survive an explicit off.
    let (_root, resolver, _mounts, request) = session_fixture(
        &["opt-on"],
        &[("opt-on", false, Some(true))],
        &[("opt-on", false)],
    );
    let mounted = resolver.mounted_plugins(&request).unwrap();
    assert!(mounted_ids(&mounted).is_empty(), "{:?}", mounted.reasons);
    assert!(
        mounted
            .reasons
            .iter()
            .any(|reason| reason.contains("switched off")),
        "{:?}",
        mounted.reasons
    );
}

#[test]
fn a_required_plugin_from_the_pilot_graph_cannot_be_switched_off() {
    let (_root, mut resolver, _mounts, request) =
        session_fixture(&["must"], &[("must", true, None)], &[("must", false)]);
    resolver.pilot_graph = Some("session-mounts".into());
    let mounted = resolver.mounted_plugins(&request).unwrap();
    assert_eq!(mounted_ids(&mounted), ["must"]);
    assert!(
        !mounted
            .reasons
            .iter()
            .any(|reason| reason.contains("`must`")),
        "a mounted Plugin needs no reason: {:?}",
        mounted.reasons
    );
}

/// The assistant can create or edit a Graph in this Session, so a
/// `required: true` in *another* Graph must not be an authorization: it is a
/// default, and the Session's switch still wins.
#[test]
fn a_required_declaration_from_another_graph_is_only_a_default() {
    let (_root, mut resolver, _mounts, request) =
        session_fixture(&["must"], &[("must", true, None)], &[("must", false)]);
    resolver.pilot_graph = Some("the-pilot-graph".into());
    let mounted = resolver.mounted_plugins(&request).unwrap();
    assert!(
        mounted_ids(&mounted).is_empty(),
        "another Graph's required must not defeat the Session: {:?}",
        mounted.reasons
    );
    assert!(
        mounted
            .reasons
            .iter()
            .any(|reason| reason.contains("treated as optional")),
        "{:?}",
        mounted.reasons
    );
    assert!(
        mounted
            .reasons
            .iter()
            .any(|reason| reason.contains("switched off")),
        "{:?}",
        mounted.reasons
    );
}

#[tokio::test]
async fn an_uninstalled_plugin_is_not_mounted_and_says_so() {
    let (_root, resolver, _mounts, request) =
        session_fixture(&["installed"], &[], &[("ghost", true), ("installed", true)]);
    let mounted = resolver.mounted_plugins(&request).unwrap();
    assert_eq!(mounted_ids(&mounted), ["installed"]);
    assert!(
        mounted
            .reasons
            .iter()
            .any(|reason| reason.contains("`ghost`")
                && reason.contains("is not available in this Library")),
        "{:?}",
        mounted.reasons
    );
    // The same reason reaches the Run's own record of what this call observed.
    resolver.tools(&request).await.unwrap();
    let observed = resolver.observed_plugin_views(&request.key);
    assert!(
        observed.get(MOUNT_EVIDENCE_KEY).is_some_and(|value| {
            value.contains("this Turn's snapshot")
                && value.contains("`ghost` is not available in this Library")
                // A Plugin mounted by a click is named as such, so "why is this
                // mounted" is answerable without reading the absence of reasons.
                && value.contains("by-switch=[installed]")
        }),
        "{observed:?}"
    );
}

#[test]
fn a_turn_keeps_the_first_snapshot_when_the_session_switch_changes() {
    let (_root, resolver, mounts, request) = session_fixture(&["side"], &[], &[("side", true)]);
    assert_eq!(
        mounted_ids(&resolver.mounted_plugins(&request).unwrap()),
        ["side"]
    );
    mounts.set("side", false);
    // The Turn already started: the tool set must not change under it.
    assert_eq!(
        mounted_ids(&resolver.mounted_plugins(&request).unwrap()),
        ["side"]
    );
    let path = plugin_mounts::snapshot_path(
        resolver.local_inputs.state_root(),
        "session-one",
        "turn-one",
    );
    assert_eq!(
        plugin_mounts::read_snapshot(&path, "session-one", "turn-one")
            .unwrap()
            .mounts,
        BTreeMap::from([("side".to_owned(), true)])
    );
}
