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
    assert_eq!(names, ["anchor_run", SEARCH_TOOL, CALL_TOOL]);
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
    let failure = port
        .call(CALL_TOOL, json!({"name": "not_mounted"}))
        .await
        .expect_err("unknown tools are refused");
    assert!(failure.to_string().contains("anchor_tools"), "{failure}");
    let failure = port
        .call(CALL_TOOL, json!({}))
        .await
        .expect_err("a name is required");
    assert!(failure.to_string().contains("anchor_tools"), "{failure}");
    assert!(inner.calls().is_empty(), "nothing may reach the port");
}

#[test]
fn read_only_is_only_claimed_for_search() {
    let (port, _) = wrapped();
    assert!(port.is_read_only(SEARCH_TOOL));
    assert!(!port.is_read_only(CALL_TOOL));
    assert!(!port.is_read_only("plugin_report"));
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
