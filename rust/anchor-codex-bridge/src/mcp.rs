use crate::manager::Manager;
use rmcp::{
    RoleServer, ServerHandler,
    model::{
        CallToolRequestParams, CallToolResult, ErrorData, Implementation, ListToolsResult,
        PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
    },
    service::RequestContext,
};
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(Clone)]
pub(crate) struct CodexService(pub Arc<Manager>);

impl ServerHandler for CodexService {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_server_info(
            Implementation::new("anchor-codex-bridge", env!("CARGO_PKG_VERSION")),
        )
    }

    async fn list_tools(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let text = json!({"type":"string"});
        let effort = json!({"type":"string","minLength":1,"description":"Native Codex reasoning effort, e.g. max. Passed unchanged; supported values depend on the local model. Omit for native inheritance; configure leaves omitted fields unchanged. Never append to model name."});
        let mode =
            json!({"type":"string","enum":["read-only","workspace-write"],"default":"read-only"});
        let wait = json!({"type":"integer","minimum":0,"maximum":30,"default":12});
        let approval =
            json!({"type":"string","enum":["on-request","never"],"default":"on-request"});
        let cursor = json!({"type":"string","description":"Opaque nextCursor from the previous page of the same query."});
        let limit = json!({"type":"integer","minimum":1,"maximum":100,"default":20});
        let tools = [
            ("projects", "Discover Git projects inside the configured local scope. Non-Git directories can also be used by spawn with an explicit project path.",
                json!({"type":"object","properties":{},"additionalProperties":false})),
            ("spawn", "Start a local Codex worker, optionally in a managed Git worktree. Returns promptly after bounded wait; running is not completion. Worktrees start at committed HEAD, not uncommitted files. Local Codex owns model credentials.",
                json!({"type":"object","properties":{"project":text,"prompt":text,"title":text,"worktree":text,"base_ref":text,"model":text,"reasoning_effort":effort,"mode":mode,"approval_policy":approval,"network_access":{"type":"boolean","default":false},"wait_seconds":wait},"required":["project","prompt"],"additionalProperties":false})),
            ("btw", "Ask a temporary side question using a scoped main session's native context, including unloaded historical sessions. Forks an ephemeral side thread without loading, steering or interrupting the parent. Returns a separate thread_id and parent_thread_id, plus any answer after bounded wait. Use inspect for unfinished answers, steer_or_resume on the SIDE id for follow-ups, and control close when done. Fork-time context is not a continuous live feed; use session fork for a persistent branch.",
                json!({"type":"object","properties":{"thread_id":text,"prompt":text,"wait_seconds":wait},"required":["thread_id","prompt"],"additionalProperties":false})),
            ("board", "Inspect live workers and a page of local history. Poll through graph-tasks for proactive progress. history_next_cursor pages stored history; imported sessions are not attached to a running CLI.",
                json!({"type":"object","properties":{"project":text,"cursor":text,"archived":{"type":"boolean"}},"additionalProperties":false})),
            ("inspect", "Read worker output, plan, command tail, git changes and pending approval/questions. History is read from Codex without loading or starting a turn. Use history for paged turns/items; inspect and board observe lifecycle operations. Never replay uncertain work.",
                json!({"type":"object","properties":{"thread_id":text},"required":["thread_id"],"additionalProperties":false})),
            ("steer_or_resume", "Steer an active turn, load an idle historical thread if needed and start its next turn, or answer one pending request. Submit request_key and explicit user-authorized answers/decision without prompt to answer; otherwise submit the real task prompt. Select authorized mode/network_access explicitly when starting work; steering does not change active permissions. Imported history defaults read-only. Use session load to load without a turn, never a dummy prompt. Never silently approve escalation.",
                json!({"type":"object","properties":{"thread_id":text,"prompt":text,"request_key":text,"approval_decision":{"type":"string","enum":["accept","acceptForSession","decline","cancel"]},"answers":{"type":"object","additionalProperties":{"type":"array","items":text}},"elicitation_content":{"type":"object"},"mode":mode,"approval_policy":approval,"network_access":{"type":"boolean","default":false},"wait_seconds":wait},"required":["thread_id"],"additionalProperties":false})),
            ("control", "Interrupt a turn, archive an idle thread, remove its managed worktree, or close a temporary btw conversation. Closing a side conversation never interrupts its parent. Removal refuses active workers and dirty files unless force is explicitly authorized; retains branch.",
                json!({"type":"object","properties":{"thread_id":text,"action":{"type":"string","enum":["interrupt","archive","remove_worktree","close"]},"force":{"type":"boolean","default":false}},"required":["thread_id","action"],"additionalProperties":false})),
            ("models", "Discover the local native Codex model catalog and reasoning efforts without starting inference. Returns native data and nextCursor; listing a model is not proof of inference access. Page with cursor; hidden entries are excluded by default. Does not change global configuration.",
                json!({"type":"object","properties":{"cursor":cursor,"limit":limit,"include_hidden":{"type":"boolean","default":false}},"additionalProperties":false})),
            ("history", "Read a scoped session's native paged history without loading, steering or starting a turn. Returns native data and nextCursor. Defaults to turns; use view items with a turn_id to inspect that turn's items. Keep cursors tied to the same thread/view/turn query.",
                json!({
                    "type":"object",
                    "properties":{
                        "thread_id":text,
                        "view":{"type":"string","enum":["turns","items"],"default":"turns"},
                        "turn_id":{"type":"string","description":"Native turn identifier; only accepted with view items."},
                        "cursor":cursor,
                        "limit":limit
                    },
                    "required":["thread_id"],
                    "allOf":[{"if":{"required":["turn_id"]},"then":{"properties":{"view":{"const":"items"}},"required":["view"]}}],
                    "additionalProperties":false
                })),
            ("session", "Manage a scoped native session using the listed convenience actions; discover other reviewed project-scoped operations with native_discover and use native_call only when allowed. load uses native resume without turn/start; unload requires idle. fork creates a persistent native branch, read-only by default, optionally in a managed worktree, without starting a turn. rename changes only the title; configure sets model/effort for NEXT turns, never global config; unarchive only unarchives. compact requires idle and STARTS native asynchronous compaction: report requested, not completed; observe with inspect/board. No permission arguments; use steer_or_resume with explicit authorized mode/network_access for actual work. Native errors must not be bypassed.",
                json!({
                    "type":"object",
                    "properties":{
                        "thread_id":text,
                        "action":{"type":"string","enum":["load","unload","fork","rename","configure","unarchive","compact"]},
                        "title":{"type":"string","description":"Session title for rename or fork only."},
                        "model":{"type":"string","description":"Native model identifier for configure or fork only."},
                        "reasoning_effort":effort,
                        "worktree":{"type":"string","description":"Managed Git worktree name for fork only; committed files, not parent uncommitted edits."},
                        "base_ref":{"type":"string","description":"Git base reference for fork's managed worktree only."}
                    },
                    "required":["thread_id","action"],
                    "allOf":[
                        {"if":{"required":["title"]},"then":{"properties":{"action":{"enum":["rename","fork"]}}}},
                        {"if":{"anyOf":[{"required":["model"]},{"required":["reasoning_effort"]}]},"then":{"properties":{"action":{"enum":["configure","fork"]}}}},
                        {"if":{"anyOf":[{"required":["worktree"]},{"required":["base_ref"]}]},"then":{"properties":{"action":{"const":"fork"}}}}
                    ],
                    "additionalProperties":false
                })),
            ("native_discover", "Discover the actual installed Codex app-server schemas and Bridge policy (allowed/boundary), not a promise that every RPC is callable. Choose request, notification or server_request; filter by exact method or search and page with offset/limit. Host-only, authentication, global configuration, unsandboxed host shell and unreviewed operations are unavailable. Notification schemas describe observations, not callable requests. Restart the Bridge after updating Codex to refresh its schema, then rediscover.",
                json!({
                    "type":"object",
                    "properties":{
                        "direction":{"type":"string","enum":["request","notification","server_request"],"default":"request"},
                        "method":{"type":"string","description":"Exact native method name."},
                        "search":{"type":"string","description":"Search the installed native catalog."},
                        "offset":{"type":"integer","minimum":0,"default":0},
                        "limit":{"type":"integer","minimum":1,"maximum":100,"default":50}
                    },
                    "additionalProperties":false
                })),
            ("native_call", "Call an allowed, schema-validated project-scoped native request and return its raw native result. Discover first; preserve native camelCase parameter names and shapes, without wrapper snake_case translation. thread/start requires an explicit native cwd inside the authorized scope. turn/start returns immediately after the native RPC response, without waiting for completion. Prefer daily wrappers; use this entry for allowed features not prewrapped. confirm is an explicit authorized confirmation where policy requires it, never an approval grant or permission bypass. Local trusted Codex owns model configuration; no global config edits, authentication management, host-only or unreviewed operations.",
                json!({
                    "type":"object",
                    "properties":{
                        "method":text,
                        "params":{"type":"object","default":{},"description":"Parameters matching the discovered native schema, with original camelCase keys and nested shapes."},
                        "confirm":{"type":"boolean","default":false}
                    },
                    "required":["method"],
                    "additionalProperties":false
                })),
            ("native_events", "Pull a bounded page of raw events for one authorized thread. after is an exclusive u64 cursor; omit it for retained events. Keep thread_id, stream_epoch and next_cursor together. Events are ephemeral and bounded, not durable history: gap or an epoch change requires history/inspect reconciliation before continuing, never automatic replay. Pull through existing authorized Graph Tasks for monitoring; this is not an automatic push channel or a notification receipt.",
                json!({
                    "type":"object",
                    "properties":{
                        "thread_id":text,
                        "after":{"type":"integer","minimum":0,"maximum":u64::MAX},
                        "limit":{"type":"integer","minimum":1,"maximum":100,"default":50}
                    },
                    "required":["thread_id"],
                    "additionalProperties":false
                })),
            ("native_respond", "Respond to the exact live pending server request for an authorized thread. Discover direction server_request and its response schema; result must validate against that native schema and request_key must match the current live request, not just a reused wire ID. Supports permitted richer native response shapes without bypassing permission rules. Human approvals require explicit user authorization for the exact request; use steer_or_resume for approval decisions. Never auto-grant permissions, broaden a decision or replay a stale request after restart. Response delivery is not proof that the requested operation completed.",
                json!({
                    "type":"object",
                    "properties":{
                        "thread_id":text,
                        "request_key":{"type":"string","description":"Exact live pending request key returned by the Bridge."},
                        "result":{"type":"object","description":"Native response object matching the discovered response schema and explicit user authorization."}
                    },
                    "required":["thread_id","request_key","result"],
                    "additionalProperties":false
                })),
        ].into_iter().map(|(name, description, schema)| Tool::new(name, description, Arc::new(schema.as_object().unwrap().clone()))).collect();
        Ok(ListToolsResult {
            tools,
            next_cursor: None,
            meta: None,
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let parts = context
            .extensions
            .get::<http::request::Parts>()
            .ok_or_else(|| ErrorData::invalid_params("HTTP request context is required", None))?;
        let scope = parts
            .headers
            .get("x-codex-project-scope")
            .map(|value| value.to_str())
            .transpose()
            .map_err(|_| ErrorData::invalid_params("invalid project scope header", None))?;
        let scope = scope.map(str::to_owned);
        let manager = self.0.clone();
        // A closed chat/tool connection must not cancel partially admitted work.
        // The caller reconciles thread history after an uncertain response.
        let result = tokio::spawn(async move {
            manager
                .dispatch(
                    &request.name,
                    Value::Object(request.arguments.unwrap_or_default()),
                    scope.as_deref(),
                )
                .await
        })
        .await
        .map_err(|_| {
            ErrorData::internal_error("bridge operation failed; inspect before retry", None)
        })?;
        match result {
            Ok(value) => Ok(CallToolResult::structured(value)),
            Err(reason) => Ok(CallToolResult::structured_error(
                json!({"status":"error","error":reason}),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BridgeConfig;
    use rmcp::{ServiceExt, model::ClientInfo};

    fn check_arguments(schema: &Value, valid: &[Value], invalid: &[Value]) {
        let validator = jsonschema::validator_for(schema).unwrap();
        for arguments in valid {
            assert!(validator.is_valid(arguments), "rejected {arguments}");
        }
        for arguments in invalid {
            assert!(!validator.is_valid(arguments), "accepted {arguments}");
        }
    }

    #[tokio::test]
    async fn native_tool_inventory_publishes_strict_envelopes_and_native_payloads() {
        let root = tempfile::tempdir().unwrap();
        let manager = Manager::new(BridgeConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            bearer_token: "native-inventory-fixture-token-not-a-credential".into(),
            allow_roots: vec![root.path().to_path_buf()],
            worktree_root: root.path().join("worktrees"),
            codex_bin: root.path().join("must-not-start-codex"),
            codex_args: Vec::new(),
        });
        let (client_io, server_io) = tokio::io::duplex(65_536);
        let handler = CodexService(manager.clone());
        let server = tokio::spawn(async move {
            handler
                .serve(server_io)
                .await
                .unwrap()
                .waiting()
                .await
                .unwrap();
        });
        let client = ClientInfo::default().serve(client_io).await.unwrap();
        let inventory = client.list_tools(None).await.unwrap();
        client.cancel().await.unwrap();
        server.await.unwrap();
        manager.shutdown().await;

        assert_eq!(inventory.tools.len(), 14);
        assert!(inventory.next_cursor.is_none());
        let schema = |name: &str| {
            Value::Object(
                inventory
                    .tools
                    .iter()
                    .find(|tool| tool.name == name)
                    .unwrap()
                    .input_schema
                    .as_ref()
                    .clone(),
            )
        };
        let discover = schema("native_discover");
        assert_eq!(discover["properties"]["direction"]["default"], "request");
        assert_eq!(discover["properties"]["offset"]["default"], 0);
        assert_eq!(discover["properties"]["limit"]["default"], 50);
        check_arguments(
            &discover,
            &[
                json!({}),
                json!({"direction":"request","method":"thread/start","offset":0,"limit":1}),
                json!({"direction":"notification","search":"turn","offset":50,"limit":100}),
                json!({"direction":"server_request","method":"item/tool/requestUserInput"}),
            ],
            &[
                json!({"direction":"response"}),
                json!({"offset":-1}),
                json!({"offset":0.5}),
                json!({"limit":0}),
                json!({"limit":101}),
                json!({"limit":1.5}),
                json!({"method":null}),
                json!({"search":[]}),
                json!({"cursor":"history-cursor"}),
            ],
        );

        let call = schema("native_call");
        assert_eq!(call["properties"]["params"]["default"], json!({}));
        assert_eq!(call["properties"]["confirm"]["default"], false);
        // The envelope permits native shapes; the installed schema and Manager own payload policy.
        check_arguments(
            &call,
            &[
                json!({"method":"model/list"}),
                json!({"method":"turn/start","params":{"threadId":"thread-1","input":[{"type":"text","text":"inspect"}],"sandboxPolicy":{"type":"readOnly"}},"confirm":false}),
                json!({"method":"thread/delete","params":{"threadId":"thread-1"},"confirm":true}),
            ],
            &[
                json!({}),
                json!({"method":1}),
                json!({"method":"model/list","params":[]}),
                json!({"method":"model/list","params":null}),
                json!({"method":"model/list","confirm":"true"}),
                json!({"method":"turn/start","wait_seconds":12}),
                json!({"method":"thread/start","project":"wrapper-project"}),
            ],
        );

        let events = schema("native_events");
        assert_eq!(events["properties"]["limit"]["default"], 50);
        assert_eq!(events["properties"]["after"]["maximum"], json!(u64::MAX));
        assert!(events["properties"]["after"].get("default").is_none());
        check_arguments(
            &events,
            &[
                json!({"thread_id":"thread-1"}),
                json!({"thread_id":"thread-1","after":0,"limit":1}),
                json!({"thread_id":"thread-1","after":u64::MAX,"limit":100}),
            ],
            &[
                json!({}),
                json!({"threadId":"thread-1"}),
                json!({"thread_id":"thread-1","after":-1}),
                json!({"thread_id":"thread-1","after":0.5}),
                json!({"thread_id":"thread-1","after":"0"}),
                json!({"thread_id":"thread-1","limit":0}),
                json!({"thread_id":"thread-1","limit":101}),
            ],
        );

        let respond = schema("native_respond");
        assert!(respond["properties"]["result"].get("default").is_none());
        check_arguments(
            &respond,
            &[
                json!({"thread_id":"thread-1","request_key":"live:key","result":{"answers":{"question-id":{"answers":["A"]}}}}),
                json!({"thread_id":"thread-1","request_key":"live:key","result":{}}),
            ],
            &[
                json!({"thread_id":"thread-1","result":{}}),
                json!({"request_key":"live:key","result":{}}),
                json!({"thread_id":"thread-1","request_key":"live:key"}),
                json!({"thread_id":"thread-1","request_key":"live:key","result":[]}),
                json!({"thread_id":"thread-1","request_key":"live:key","result":null}),
                json!({"thread_id":"thread-1","request_key":"live:key","result":{},"confirm":true}),
            ],
        );
    }
}
