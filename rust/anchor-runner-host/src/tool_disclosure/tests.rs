use super::*;
use std::sync::Mutex;

/// A port that records what it was asked to call.
struct Fake {
    definitions: Vec<ToolDefinition>,
    calls: Mutex<Vec<(String, Value)>>,
}

impl Fake {
    fn new() -> Self {
        let definition = |name: &str, description: &str| {
            ToolDefinition::new(
                ToolName::new(name).expect("tool name"),
                description,
                json!({"type": "object"}),
            )
        };
        Self {
            definitions: vec![
                definition("anchor_run", "Run an authorized command as an argv array."),
                definition(
                    "plugin_report",
                    "Build the weekly report from the configured source.\nSecond line is not the summary.",
                ),
                definition("plugin_mail", "Send mail through the tenant gateway."),
            ],
            calls: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<(String, Value)> {
        self.calls.lock().unwrap().clone()
    }
}

impl ToolPort for Fake {
    fn definitions(&self) -> Vec<ToolDefinition> {
        self.definitions.clone()
    }

    fn is_read_only(&self, name: &str) -> bool {
        name == "plugin_report"
    }

    fn call<'a>(
        &'a self,
        name: &'a str,
        arguments: Value,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<Vec<ToolResultContent>, ToolError>> + Send + 'a,
        >,
    > {
        Box::pin(async move {
            self.calls
                .lock()
                .unwrap()
                .push((name.to_owned(), arguments.clone()));
            if name == "missing" {
                return Err(ToolError::Unknown(name.to_owned()));
            }
            Ok(vec![ToolResultContent::json(json!({"tool": name}))])
        })
    }
}

fn wrapped() -> (Arc<dyn ToolPort>, Arc<Fake>) {
    let inner = Arc::new(Fake::new());
    let port = wrap(inner.clone(), DisclosurePolicy::new(["anchor_run"]));
    (port, inner)
}

async fn call(port: &Arc<dyn ToolPort>, name: &str, arguments: Value) -> Value {
    port.call(name, arguments).await.expect("call succeeds")[0]
        .as_json()
        .expect("json result")
        .clone()
}

#[test]
fn only_declared_tools_are_advertised() {
    let (port, _) = wrapped();
    let mut names = port
        .definitions()
        .into_iter()
        .map(|definition| definition.name)
        .collect::<Vec<_>>();
    names.sort();
    assert_eq!(
        names,
        [
            plugin_disclosure::READ_TOOL,
            plugin_disclosure::LIST_TOOL,
            "anchor_run",
            DETAIL_TOOL,
            SEARCH_TOOL,
            CALL_TOOL
        ]
    );
}

#[tokio::test]
async fn search_lists_every_callable_tool_with_paging() {
    let (port, _) = wrapped();
    let page = call(&port, SEARCH_TOOL, json!({"limit": 2})).await;
    assert_eq!(page["total"], 3, "{page}");
    assert_eq!(page["returned"], 2, "{page}");
    assert_eq!(page["truncated"], true, "{page}");
    assert_eq!(page["next_offset"], 2, "{page}");
    assert_eq!(page["tools"][0]["name"], "anchor_run", "{page}");
    assert_eq!(page["tools"][0]["read_only"], false, "{page}");
    assert_eq!(page["tools"][1]["name"], "plugin_mail", "{page}");
    assert!(
        page["tools"][1]["summary"]
            .as_str()
            .unwrap()
            .starts_with("Send mail"),
        "{page}"
    );

    let rest = call(&port, SEARCH_TOOL, json!({"offset": 2, "limit": 50})).await;
    assert_eq!(rest["returned"], 1, "{rest}");
    assert_eq!(rest["tools"][0]["name"], "plugin_report", "{rest}");
    assert_eq!(rest["tools"][0]["read_only"], true, "{rest}");
    assert!(
        rest["tools"][0]["summary"]
            .as_str()
            .unwrap()
            .contains("weekly report"),
        "summary is the first line only: {rest}"
    );
}

#[tokio::test]
async fn search_narrows_by_query_and_bounds_the_page() {
    let (port, _) = wrapped();
    let filtered = call(&port, SEARCH_TOOL, json!({"query": "MAIL"})).await;
    assert_eq!(filtered["total"], 1, "{filtered}");
    assert_eq!(filtered["tools"][0]["name"], "plugin_mail", "{filtered}");

    let bounded = call(&port, SEARCH_TOOL, json!({"limit": 10_000})).await;
    assert_eq!(bounded["returned"], 3, "{bounded}");
}

#[tokio::test]
async fn invoke_delegates_and_returns_the_inner_result_unchanged() {
    let (port, inner) = wrapped();
    let result = call(
        &port,
        CALL_TOOL,
        json!({"name": "plugin_mail", "arguments": {"to": "ops"}}),
    )
    .await;
    assert_eq!(result, json!({"tool": "plugin_mail"}));
    assert_eq!(
        inner.calls(),
        vec![("plugin_mail".to_owned(), json!({"to": "ops"}))]
    );
    // A declared tool still works directly, without the indirection.
    let direct = call(&port, "anchor_run", json!({"command": ["true"]})).await;
    assert_eq!(direct, json!({"tool": "anchor_run"}));
}

#[tokio::test]
async fn invoke_refuses_meta_tools_unknown_names_and_missing_arguments() {
    let (port, inner) = wrapped();
    let failure = port
        .call(CALL_TOOL, json!({"name": SEARCH_TOOL}))
        .await
        .expect_err("meta tools are not callable through the indirection");
    assert!(failure.to_string().contains("disclosure tool"), "{failure}");
    for name in ["not_mounted", "web.search"] {
        for entrypoint in [DETAIL_TOOL, CALL_TOOL] {
            let failure = port
                .call(entrypoint, json!({"name": name}))
                .await
                .expect_err("unknown tools are refused");
            let message = failure.to_string();
            assert!(message.contains("use anchor_tools"), "{failure}");
            assert!(message.contains("current authorized names"), "{failure}");
            for advertised in ["plugin_mail", "plugin_report", "anchor_run"] {
                assert!(!message.contains(advertised), "{failure}");
            }
        }
    }
    let failure = port
        .call(CALL_TOOL, json!({}))
        .await
        .expect_err("a name is required");
    assert!(failure.to_string().contains("anchor_tools"), "{failure}");
    assert!(inner.calls().is_empty(), "nothing may reach the port");
}

#[test]
fn read_only_includes_discovery_and_the_real_target_flags() {
    let (port, _) = wrapped();
    assert!(port.is_read_only(SEARCH_TOOL));
    assert!(!port.is_read_only(CALL_TOOL));
    assert!(port.is_read_only("plugin_report"));
}

#[test]
fn persistent_summary_matches_filtered_tool_owners_in_both_schema_modes() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("plugins/demo");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join("plugin.json"),
        r#"{"name":"Report Plugin","description":"Create weekly reports"}"#,
    )
    .unwrap();
    let mut catalog = PluginDisclosureCatalog::default();
    catalog.insert(
        crate::plugin_library::Catalog::new(root.path())
            .definition("demo")
            .unwrap(),
    );
    catalog.attach_tools(
        ["plugin_report", "not_callable"]
            .into_iter()
            .map(|name| {
                (
                    name.into(),
                    plugin_disclosure::ToolOwner {
                        plugin: "demo".into(),
                        server: "reports".into(),
                        tool: name.into(),
                    },
                )
            })
            .collect(),
    );
    let mut disclosure = ToolDisclosure::new(
        Fake::new().definitions,
        BTreeMap::new(),
        DisclosurePolicy::new(["anchor_run"]),
        catalog,
    );
    let expected = disclosure.instructions();
    for enabled in [false, true] {
        disclosure.enabled = enabled;
        assert_eq!(disclosure.instructions(), expected);
        assert!(
            disclosure
                .definitions()
                .iter()
                .any(|tool| tool.name == plugin_disclosure::LIST_TOOL)
        );
        let entry: Value = serde_json::from_str(expected.lines().nth(1).unwrap()).unwrap();
        assert_eq!(entry["tool_count"], 1);
        let listed = disclosure
            .inspect(plugin_disclosure::LIST_TOOL, &json!({"plugin":"demo"}))
            .unwrap();
        assert_eq!(
            listed[0].as_json().unwrap()["plugins"][0]["tool_count"],
            entry["tool_count"]
        );
    }
}

#[tokio::test]
async fn summaries_are_truncated_to_one_bounded_line() {
    let long = "x".repeat(400);
    let inner = Arc::new(Fake {
        definitions: vec![ToolDefinition::new(
            ToolName::new("plugin_long").expect("tool name"),
            long.clone(),
            json!({"type": "object"}),
        )],
        calls: Mutex::new(Vec::new()),
    });
    let port = wrap(inner, DisclosurePolicy::new(Vec::<String>::new()));
    let page = call(&port, SEARCH_TOOL, json!({})).await;
    let summary = page["tools"][0]["summary"].as_str().unwrap();
    assert_eq!(summary.chars().count(), SUMMARY_CHARS + 1, "{summary}");
    assert!(summary.ends_with('…'), "{summary}");
}

/// A plugin-shaped tool: name, a two-line description and a small argument schema.
fn realistic(name: &str, index: usize) -> ToolDefinition {
    ToolDefinition::new(
        ToolName::new(name).expect("tool name"),
        format!(
            "Perform capability number {index} against the configured tenant resource.\n\
             Use when the task needs capability {index}; returns a structured result."
        ),
        json!({
            "type": "object",
            "properties": {
                "target": {"type": "string", "description": "resource identifier"},
                "options": {
                    "type": "object",
                    "properties": {
                        "limit": {"type": "integer", "description": "maximum rows"},
                        "mode": {"type": "string", "enum": ["fast", "thorough"]}
                    }
                }
            },
            "required": ["target"],
            "additionalProperties": false
        }),
    )
}

fn schema_bytes(definitions: &[ToolDefinition]) -> usize {
    serde_json::to_vec(definitions)
        .expect("serialize definitions")
        .len()
}

#[test]
fn disclosure_keeps_the_advertised_schema_bounded() {
    // What a model request carries is exactly the advertised definitions, so this
    // is the per-request schema cost for a node with `count` mounted tools.
    let mut reported = Vec::new();
    for count in [10usize, 100, 1_000] {
        let inner = Arc::new(Fake {
            definitions: (0..count)
                .map(|i| realistic(&format!("plugin_tool_{i}"), i))
                .collect(),
            calls: Mutex::new(Vec::new()),
        });
        let raw = schema_bytes(&inner.definitions());
        let disclosed =
            schema_bytes(&wrap(inner, DisclosurePolicy::new(["anchor_run"])).definitions());
        reported.push((count, raw, disclosed));
        println!(
            "tools={count:>4}  raw={raw:>7}B (~{:>5} tok)  disclosed={disclosed:>5}B (~{:>4} tok)",
            raw / 4,
            disclosed / 4
        );
    }
    // The advertised set is always the declared tools plus the five discovery tools, so it
    // cannot grow with the number of mounted tools.
    let disclosed = reported
        .iter()
        .map(|(_, _, disclosed)| *disclosed)
        .collect::<Vec<_>>();
    assert!(
        disclosed.windows(2).all(|pair| pair[0] == pair[1]),
        "disclosed schema size must not depend on the mounted tool count: {disclosed:?}"
    );
    let (count, raw, disclosed) = *reported.last().expect("measured");
    assert!(
        raw > disclosed * 20,
        "{count} tools must cost far more undiscosed ({raw}B) than disclosed ({disclosed}B)"
    );
}

#[test]
fn always_visible_accepts_a_host_override() {
    assert_eq!(
        parse_always_visible(r#"["anchor_run"]"#).unwrap(),
        vec!["anchor_run".to_owned()]
    );
    assert_eq!(
        parse_always_visible("[]").unwrap(),
        Vec::<String>::new(),
        "a host may hide everything behind the disclosure tools"
    );
    assert!(
        parse_always_visible("anchor_run").is_none(),
        "a malformed override falls back to the node's defaults"
    );
}

#[test]
fn disclosure_waits_until_the_meta_tools_pay_for_themselves() {
    assert!(!should_disclose(None, 0));
    assert!(!should_disclose(None, MIN_HIDDEN_FOR_DISCLOSURE));
    assert!(should_disclose(None, MIN_HIDDEN_FOR_DISCLOSURE + 1));
    assert!(should_disclose(Some("1"), 0), "the host can force it");
    assert!(!should_disclose(Some("0"), 100), "the host can forbid it");
    assert!(
        should_disclose(Some("nonsense"), MIN_HIDDEN_FOR_DISCLOSURE + 1),
        "an unrecognised value falls back to the measured rule"
    );
}

#[tokio::test]
async fn schema_is_complete_and_search_supports_multiple_chinese_terms() {
    let definition = ToolDefinition::new(
        ToolName::new("research_cn").unwrap(),
        "中文证据 搜索 nested options",
        json!({"type":"object","properties":{"items":{"type":"array","items":{"oneOf":[{"type":"string"},{"type":"object","properties":{"id":{"type":"integer"}},"required":["id"]}]}}},"required":["items"],"additionalProperties":false}),
    );
    let inner = Arc::new(Fake {
        definitions: vec![definition.clone()],
        calls: Mutex::new(Vec::new()),
    });
    let port = wrap(inner, DisclosurePolicy::new(Vec::<String>::new()));
    let page = call(&port, SEARCH_TOOL, json!({"query":"中文 搜索"})).await;
    assert_eq!(page["total"], 1);
    let detail = call(&port, DETAIL_TOOL, json!({"name":"research_cn"})).await;
    assert_eq!(detail["parameters"], definition.parameters);
    assert_eq!(detail["complete"], true);
    assert!(
        port.call(CALL_TOOL, json!({"name":"final_result","arguments":{}}))
            .await
            .is_err()
    );
    assert!(
        port.call(CALL_TOOL, json!({"name":"research_cn","arguments":[]}))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn oversized_schema_returns_a_clear_error() {
    let definition = ToolDefinition::new(
        ToolName::new("large").unwrap(),
        "Large",
        json!({"type":"object","description":"x".repeat(plugin_disclosure::DETAIL_BYTES)}),
    );
    let inner = Arc::new(Fake {
        definitions: vec![definition],
        calls: Mutex::new(Vec::new()),
    });
    let port = wrap(inner, DisclosurePolicy::new(Vec::<String>::new()));
    assert!(
        port.call(DETAIL_TOOL, json!({"name":"large"}))
            .await
            .unwrap_err()
            .to_string()
            .contains("complete schema/instruction was not returned")
    );
}
