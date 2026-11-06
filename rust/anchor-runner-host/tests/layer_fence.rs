mod common;

use std::{collections::BTreeMap, path::Path};

const RULES: [&str; 3] = [
    "service_application",
    "service_adapter",
    "application_adapter",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Layer {
    Composition,
    Adapter,
    Application,
    Service,
}

fn module(path: &str, name: &str) -> bool {
    path.strip_prefix(name)
        .is_some_and(|suffix| suffix == ".rs" || suffix.starts_with('/'))
}

// These are architectural roles, not baseline exemptions. In particular,
// node_host remains a service even though the Run fence treats it as runtime.
fn layer(path: &str) -> Layer {
    if matches!(
        path,
        "main.rs" | "execution.rs" | "execution/tool_assembly.rs"
    ) || path.starts_with("bin/")
    {
        Layer::Composition
    } else if ["api", "pilot_tools", "node_tools", "graph_task_tools"]
        .iter()
        .any(|name| module(path, name))
        || path == "channel_tools.rs"
    {
        Layer::Adapter
    } else if module(path, "application") {
        Layer::Application
    } else {
        Layer::Service
    }
}

fn target_layer(name: &str) -> Layer {
    match name {
        "application" => Layer::Application,
        "api" | "pilot_tools" | "node_tools" | "channel_tools" | "graph_task_tools" => {
            Layer::Adapter
        }
        _ => Layer::Service,
    }
}

fn path_head<'a>(tokens: &[&'a str]) -> Option<&'a str> {
    if tokens.starts_with(&["r", "#"]) {
        tokens.get(2).copied()
    } else {
        tokens.first().copied()
    }
}

// Count lexical crate-root paths, including imports, aliases, visibility paths
// and macro input. Also handle crate::{module, module::{...}} without counting
// nested names as root modules. No name resolution, alias-use tracking or macro
// expansion; relative paths and glob imports are not resolved.
fn count(path: &str, tokens: &[&str]) -> [usize; 3] {
    let source = layer(path);
    let mut counts = [0; 3];
    let mut record = |name| {
        let rule = match (source, target_layer(name)) {
            (Layer::Service, Layer::Application) => 0,
            (Layer::Service, Layer::Adapter) => 1,
            (Layer::Application, Layer::Adapter) => 2,
            _ => return,
        };
        counts[rule] += 1;
    };
    for (offset, token) in tokens.iter().enumerate() {
        if *token != "crate" || !tokens[offset + 1..].starts_with(&[":", ":"]) {
            continue;
        }
        let root = offset + 3;
        if tokens.get(root) == Some(&"{") {
            let mut depth = 0;
            let mut item = true;
            for (offset, token) in tokens[root + 1..].iter().enumerate() {
                match *token {
                    "}" if depth == 0 => break,
                    "{" => depth += 1,
                    "}" => depth -= 1,
                    "," if depth == 0 => item = true,
                    _ if depth == 0 && item => {
                        if let Some(name) = path_head(&tokens[root + 1 + offset..]) {
                            record(name);
                        }
                        item = false;
                    }
                    _ => {}
                }
            }
        } else if let Some(name) = path_head(&tokens[root..]) {
            record(name);
        }
    }
    counts
}

fn check_count(
    path: &str,
    rule: &str,
    actual: usize,
    maximum: usize,
) -> Result<Option<String>, String> {
    if actual > maximum {
        Err(format!(
            "{path} {rule} {actual} (maximum {maximum}; fix the dependency, not the baseline)"
        ))
    } else if actual < maximum {
        Ok(Some(format!(
            "layer_fence: lower layer-fence.txt: {path} {rule} {maximum} -> {actual}"
        )))
    } else {
        Ok(None)
    }
}

#[test]
fn layer_classification() {
    for path in [
        "main.rs",
        "execution.rs",
        "execution/tool_assembly.rs",
        "bin/anchor-net-relay.rs",
    ] {
        assert_eq!(layer(path), Layer::Composition, "{path}");
    }
    for path in [
        "api.rs",
        "api/runs.rs",
        "pilot_tools.rs",
        "pilot_tools/plugins.rs",
        "node_tools.rs",
        "node_tools/library.rs",
        "channel_tools.rs",
        "graph_task_tools.rs",
    ] {
        assert_eq!(layer(path), Layer::Adapter, "{path}");
    }
    for path in ["application.rs", "application/run_access/views.rs"] {
        assert_eq!(layer(path), Layer::Application, "{path}");
    }
    for path in [
        "node_host.rs",
        "node_host/preparation.rs",
        "goose_acp/pilot.rs",
        "execution/new.rs",
        "application_extra.rs",
        "api_extra.rs",
        "pilot_tools_extra.rs",
        "node_tools_extra.rs",
        "channel_tools/new.rs",
        "bin_extra.rs",
    ] {
        assert_eq!(layer(path), Layer::Service, "{path}");
    }
}

#[test]
fn dependency_counting() {
    let source = r####"
        use crate :: application :: RunApplication as App;
        use crate::{application::metadata, api::{self, tools}, nested::{application, api}};
        crate::pilot_tools::identifier(id);
        expose!(crate::node_tools::Tools);
        pub(in crate::channel_tools) fn helper() {}
        // crate::application::metadata
        /* crate::api /* crate::application */ */
        let _ = "crate::application";
        let _ = br#"crate::api"#;
        let _ = c"crate::node_tools";
        #[cfg(test)] mod fixture;
        #[cfg(test)] fn test_helper(_: crate::application::RunApplication) {}
        fn after(_: crate::application::RunApplication) {}
        othercrate::application::helper();
        crate::application_extra::helper();
    "####;
    let mut external_tests = Vec::new();
    let tokens = common::production_tokens(source, &mut external_tests);
    assert_eq!(external_tests, ["fixture"]);
    assert_eq!(count("host.rs", &tokens), [3, 4, 0]);
    assert_eq!(count("application/use_case.rs", &tokens), [0, 0, 4]);
    assert_eq!(count("api.rs", &tokens), [0, 0, 0]);
    assert_eq!(count("execution.rs", &tokens), [0, 0, 0]);
    let tokens = common::production_tokens(
        "use crate::{r#application::metadata, r#api}; crate::r#pilot_tools::helper();",
        &mut Vec::new(),
    );
    assert_eq!(count("host.rs", &tokens), [1, 2, 0]);
    assert!(check_count("new.rs", RULES[0], 1, 0).is_err());
    assert!(check_count("known.rs", RULES[1], 2, 1).is_err());
    assert_eq!(
        check_count("deleted.rs", RULES[2], 0, 1),
        Ok(Some(
            "layer_fence: lower layer-fence.txt: deleted.rs application_adapter 1 -> 0".into()
        ))
    );
    assert_eq!(check_count("known.rs", RULES[0], 1, 1), Ok(None));
}

// Only these architecture-reviewed debts may carry an inline exception name.
// Keep layer roles and per-file maxima enforced even for named debts.
fn named_exception(path: &str) -> Option<&'static str> {
    match path {
        "assistant.rs" => Some("G1-assistant-run"),
        "assistant_plugins.rs" => Some("G1-assistant-plugins"),
        "channel_supervisor/management.rs" => Some("G3-channel-management"),
        "conversation_resources.rs" => Some("G1-conversation-resources"),
        "goose_acp/pilot.rs" => Some("G8-goose-pilot"),
        "goose_acp/pilot_interaction.rs" => Some("G8-pilot-interaction"),
        "goose_acp/pilot_interaction/channel_management.rs" => Some("G8-channel-interaction"),
        "local_inputs.rs" => Some("G1-local-inputs"),
        "node_host.rs" => Some("G2-node-host"),
        "node_host/conversation.rs" => Some("G2-node-conversation"),
        "pilot_host.rs" => Some("G9-pilot-host"),
        _ => None,
    }
}

#[test]
fn layer_fence() {
    let source_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let counts = common::production_counts(&source_root, count);
    let mut baseline = BTreeMap::new();
    for line in include_str!("../layer-fence.txt").lines() {
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let (limits, explanation) = line.split_once('#').expect("missing named exception");
        let fields: Vec<_> = limits.split_whitespace().collect();
        assert_eq!(fields.len(), 4, "expected path and three counts: {line}");
        assert!(
            matches!(layer(fields[0]), Layer::Service | Layer::Application),
            "unrestricted source in baseline: {line}"
        );
        let name = explanation
            .trim()
            .split_once(':')
            .expect("missing exception reason")
            .0;
        assert_eq!(
            Some(name),
            named_exception(fields[0]),
            "unapproved exception: {line}"
        );
        let maximum: [usize; 3] =
            std::array::from_fn(|i| fields[i + 1].parse().expect("invalid baseline count"));
        assert!(
            baseline.insert(fields[0], maximum).is_none(),
            "duplicate baseline: {line}"
        );
    }
    let mut violations = Vec::new();
    let mut totals = [0; 3];
    let mut named = [0; 3];
    let mut unapproved = [0; 3];
    for (path, actual) in &counts {
        let maximum = baseline.get(path.as_str()).copied().unwrap_or([0; 3]);
        for i in 0..RULES.len() {
            totals[i] += actual[i];
            named[i] += actual[i].min(maximum[i]);
            unapproved[i] += actual[i].saturating_sub(maximum[i]);
            if let Err(message) = check_count(path, RULES[i], actual[i], maximum[i]) {
                violations.push(message);
            }
        }
    }
    for (path, maximum) in baseline {
        let actual = counts.get(path).copied().unwrap_or([0; 3]);
        for i in 0..RULES.len() {
            if let Ok(Some(message)) = check_count(path, RULES[i], actual[i], maximum[i]) {
                eprintln!("{message}");
            }
        }
    }
    eprintln!(
        "layer_fence: {} production files; {} {}; {} {}; {} {}",
        counts.len(),
        RULES[0],
        totals[0],
        RULES[1],
        totals[1],
        RULES[2],
        totals[2]
    );
    eprintln!("layer_fence: named exceptions {named:?}; unapproved {unapproved:?}");
    assert!(
        violations.is_empty(),
        "Host layer ratchet exceeded:\n{}",
        violations.join("\n")
    );
}
