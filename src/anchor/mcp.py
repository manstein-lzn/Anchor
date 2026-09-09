"""MCP server over stdio: the agent surface, not a privileged backdoor.

Every tool is a thin adapter over `anchor.client.AnchorClient`, so the MCP
server goes through the same authenticated HTTP API, the same guards and the
same audit trail as the web console. It never touches the database.

Human-only operations (approvals and waits) and operator-only operations
(retention, budgets) are refused with a structured error unless the operator
explicitly opts in, because an approval an agent can grant is not an approval.

Transport: newline-delimited JSON-RPC 2.0 on stdin/stdout. No third-party
dependency; the protocol surface Anchor needs (`initialize`, `tools/list`,
`tools/call`, `ping`) is small and stable.
"""

from __future__ import annotations

import json
import sys
from dataclasses import dataclass
from typing import Any, Callable

from anchor.client import AnchorApiError, AnchorClient
from anchor.domain.content import ARTIFACT_PREFIX

PROTOCOL_VERSION = "2024-11-05"
SERVER_NAME = "anchor"
SERVER_VERSION = "1.0.0"

# The human decision gate exists so a person, not the author, signs off. An
# agent that can call these makes the gate theatre.
HUMAN_ONLY = frozenset({"approve_wait", "reject_wait", "resume_wait"})
OPERATOR_ONLY = frozenset({"retention_sweep", "set_budget"})


@dataclass(frozen=True)
class Tool:
    name: str
    description: str
    properties: dict[str, Any]
    required: tuple[str, ...]
    handler: Callable[[AnchorClient, dict], Any]

    @property
    def restricted(self) -> bool:
        return self.name in HUMAN_ONLY or self.name in OPERATOR_ONLY

    def schema(self) -> dict:
        return {
            "name": self.name,
            "description": self.description,
            "inputSchema": {
                "type": "object",
                "properties": self.properties,
                "required": list(self.required),
                "additionalProperties": False,
            },
        }


def _str(description: str) -> dict:
    return {"type": "string", "description": description}


def _int(description: str) -> dict:
    return {"type": "integer", "description": description}


def _obj(description: str) -> dict:
    return {"type": "object", "description": description}


RUN_ID = _str("Run identifier")


def build_tools() -> list[Tool]:
    """The tool surface, grouped by intent rather than by REST resource."""
    return [
        # -- discover ------------------------------------------------------
        Tool("health", "Readiness of the API and its services.", {}, (),
             lambda c, a: c.health()),
        Tool("capabilities", "Available model, agent, tool and verifier refs.", {}, (),
             lambda c, a: c.capabilities()),
        Tool("ir", "Graph IR authoring reference: node types, condition DSL, "
                   "input mapping, template. Read this before authoring.", {}, (),
             lambda c, a: c.ir()),
        Tool("list_graphs", "List graph drafts.", {"limit": _int("Page size"), "offset": _int("Offset")},
             (), lambda c, a: c.list_graphs(limit=a.get("limit", 50), offset=a.get("offset", 0))),
        Tool("get_draft", "Current draft of a graph, or null.", {"graph_id": _str("Graph id")},
             ("graph_id",), lambda c, a: c.get_draft(a["graph_id"])),
        Tool("list_versions", "Published versions of a graph.",
             {"graph_id": _str("Graph id"), "limit": _int("Page size")}, ("graph_id",),
             lambda c, a: c.list_versions(a["graph_id"], limit=a.get("limit", 50))),
        Tool("get_version", "One published graph version.",
             {"version_id": _str("Graph version id")}, ("version_id",),
             lambda c, a: c.get_version(a["version_id"])),
        # -- author --------------------------------------------------------
        Tool("validate", "Structural validation of a definition (no persistence).",
             {"definition": _obj("Graph definition")}, ("definition",),
             lambda c, a: c.validate(a["definition"])),
        Tool("validate_capabilities", "Check agent/tool/verifier refs resolve.",
             {"definition": _obj("Graph definition")}, ("definition",),
             lambda c, a: c.validate_capabilities(a["definition"])),
        Tool("save_draft", "Save a draft at an expected revision (optimistic lock).",
             {"graph_id": _str("Graph id"), "definition": _obj("Graph definition"),
              "expected_revision": _int("Revision you read; 0 when creating")},
             ("graph_id", "definition"),
             lambda c, a: c.save_draft(a["graph_id"], a["definition"],
                                       expected_revision=a.get("expected_revision", 0))),
        Tool("publish", "Publish a draft revision as an immutable version.",
             {"graph_id": _str("Graph id"), "expected_revision": _int("Draft revision")},
             ("graph_id", "expected_revision"),
             lambda c, a: c.publish(a["graph_id"], expected_revision=a["expected_revision"])),
        Tool("install", "Validate, save and optionally publish in one call. "
                        "Prefer this over validate + save_draft + publish.",
             {"definition": _obj("Graph definition"), "publish": {"type": "boolean",
              "description": "Publish after saving (default true)"}}, ("definition",),
             lambda c, a: c.install(a["definition"], publish=a.get("publish", True))),
        Tool("export_bundle", "Export a version as a portable bundle.",
             {"version_id": _str("Graph version id")}, ("version_id",),
             lambda c, a: c.export_bundle(a["version_id"])),
        Tool("import_bundle", "Import a bundle, optionally publishing it.",
             {"bundle": _obj("Bundle from export_bundle"),
              "expected_revision": _int("Draft revision you read"),
              "publish": {"type": "boolean", "description": "Publish after import"}},
             ("bundle",),
             lambda c, a: c.import_bundle(a["bundle"],
                                          expected_revision=a.get("expected_revision", 0),
                                          publish=a.get("publish", True))),
        # -- execute -------------------------------------------------------
        Tool("register_trigger", "Register a trigger for a version.",
             {"graph_version_id": _str("Graph version id"), "type": _str("Trigger type"),
              "trigger_id": _str("Optional explicit trigger id"),
              "enabled": {"type": "boolean", "description": "Start enabled"}},
             ("graph_version_id",),
             lambda c, a: c.register_trigger(a["graph_version_id"], trigger_id=a.get("trigger_id"),
                                             type=a.get("type", "manual"),
                                             enabled=a.get("enabled", True))),
        Tool("list_triggers", "Triggers of a version.", {"version_id": _str("Graph version id")},
             ("version_id",), lambda c, a: c.list_triggers(a["version_id"])),
        Tool("set_trigger_enabled", "Enable or disable a trigger.",
             {"trigger_id": _str("Trigger id"), "enabled": {"type": "boolean",
              "description": "Desired state"}}, ("trigger_id", "enabled"),
             lambda c, a: c.set_trigger_enabled(a["trigger_id"], a["enabled"])),
        Tool("start_run", "Admit a run. Pass an idempotency_key so a retry cannot "
                          "create a second run.",
             {"trigger_id": _str("Trigger id"), "objective": _str("What the run must achieve"),
              "inputs": _obj("Run inputs"), "idempotency_key": _str("Stable key for retries")},
             ("trigger_id", "objective"),
             lambda c, a: c.start_run(a["trigger_id"], objective=a["objective"],
                                      inputs=a.get("inputs"),
                                      idempotency_key=a.get("idempotency_key"))),
        Tool("pause_run", "Pause a run; running nodes finish their attempt.",
             {"run_id": RUN_ID, "reason": _str("Why"), "actor": _str("Who")}, ("run_id", "reason"),
             lambda c, a: c.pause_run(a["run_id"], reason=a["reason"],
                                      actor=a.get("actor", "agent"))),
        Tool("resume_run", "Resume a paused run.",
             {"run_id": RUN_ID, "reason": _str("Why"), "actor": _str("Who")}, ("run_id", "reason"),
             lambda c, a: c.resume_run(a["run_id"], reason=a["reason"],
                                       actor=a.get("actor", "agent"))),
        Tool("stop_run", "Stop a run. Does not undo external side effects.",
             {"run_id": RUN_ID, "reason": _str("Why")}, ("run_id", "reason"),
             lambda c, a: c.stop_run(a["run_id"], reason=a["reason"])),
        Tool("archive_run", "Archive a terminal run (reversible; hidden from default lists).",
             {"run_id": RUN_ID}, ("run_id",), lambda c, a: c.archive_run(a["run_id"])),
        Tool("unarchive_run", "Restore an archived run.",
             {"run_id": RUN_ID}, ("run_id",), lambda c, a: c.unarchive_run(a["run_id"])),
        # -- observe -------------------------------------------------------
        Tool("run_digest", "Compact run view: status, phase, counts, blockers. "
                           "Start here, then drill down.",
             {"run_id": RUN_ID}, ("run_id",), lambda c, a: c.run_digest(a["run_id"])),
        Tool("wait_for_run", "Bounded long-poll on a run. Returns timed_out: true at the "
                             "deadline so you can loop without burning context.",
             {"run_id": RUN_ID, "timeout": {"type": "number", "description": "Seconds"},
              "interval": {"type": "number", "description": "Poll interval in seconds"}},
             ("run_id",),
             lambda c, a: c.wait_for_run(a["run_id"], timeout=a.get("timeout", 300.0),
                                         interval=a.get("interval", 2.0))),
        Tool("run_nodes", "Per-node status, attempt and error code.",
             {"run_id": RUN_ID}, ("run_id",), lambda c, a: c.run_nodes(a["run_id"])),
        Tool("run_events", "Append-only event page; resume with the last sequence you saw.",
             {"run_id": RUN_ID, "after": _int("Last sequence you saw"),
              "limit": _int("Page size")}, ("run_id",),
             lambda c, a: c.run_events(a["run_id"], after=a.get("after", 0),
                                       limit=a.get("limit", 100))),
        Tool("run_decisions", "Routing decisions taken.", {"run_id": RUN_ID}, ("run_id",),
             lambda c, a: c.run_decisions(a["run_id"])),
        Tool("run_verifications", "Verifier verdicts with evidence refs.", {"run_id": RUN_ID},
             ("run_id",), lambda c, a: c.run_verifications(a["run_id"])),
        Tool("run_operations", "Tool and external side-effect ledger.", {"run_id": RUN_ID},
             ("run_id",), lambda c, a: c.run_operations(a["run_id"])),
        Tool("run_diagnostics", "Diagnostic records for the run.", {"run_id": RUN_ID}, ("run_id",),
             lambda c, a: c.run_diagnostics(a["run_id"])),
        Tool("run_progress", "Progress observations for the run.", {"run_id": RUN_ID}, ("run_id",),
             lambda c, a: c.run_progress(a["run_id"])),
        Tool("list_waits", "Everything waiting for a human.", {}, (), lambda c, a: c.list_waits()),
        Tool("read_artifact", "Read an artifact by the content reference a node "
                              "output returned.",
             {"ref": _str(f"content reference, e.g. {ARTIFACT_PREFIX}<digest>")}, ("ref",),
             lambda c, a: c.read_artifact(a["ref"])),
        # -- reconcile -----------------------------------------------------
        Tool("reconcile_operation", "Resolve an outcome_unknown side effect. Requires "
                                    "external evidence; never guess.",
             {"operation_id": _str("Operation id"),
              "status": _str("succeeded | failed"),
              "reconciliation_ref": _str("Where the truth came from"),
              "result_ref": _str("Optional result reference"),
              "error_code": _str("Optional error code"), "reason": _str("Why")},
             ("operation_id", "status", "reconciliation_ref"),
             lambda c, a: c.reconcile_operation(a["operation_id"], status=a["status"],
                                                reconciliation_ref=a["reconciliation_ref"],
                                                result_ref=a.get("result_ref"),
                                                error_code=a.get("error_code"),
                                                reason=a.get("reason", ""))),
        # -- storage -------------------------------------------------------
        Tool("storage_report", "Storage used by the database and artifacts.", {}, (),
             lambda c, a: c.storage_report()),
        Tool("get_budget", "Current storage budgets.", {}, (), lambda c, a: c.get_budget()),
        Tool("retention_preview", "What a sweep would evict, without evicting.", {}, (),
             lambda c, a: c.retention_preview()),
        Tool("retention_audit", "Recent eviction decisions.",
             {"limit": _int("How many records")}, (),
             lambda c, a: c.retention_audit(limit=a.get("limit", 50))),
        Tool("set_budget", "OPERATOR-ONLY. Set storage budgets.",
             {"global_bytes": _int("Global cap"), "graphs": _obj("Per-graph caps")}, (),
             lambda c, a: c.set_budget(global_bytes=a.get("global_bytes"),
                                       graphs=a.get("graphs"))),
        Tool("retention_sweep", "OPERATOR-ONLY. Evict the oldest terminal runs to fit the "
                                "budget.", {}, (), lambda c, a: c.retention_sweep()),
        # -- human-in-the-loop ---------------------------------------------
        Tool("approve_wait", "HUMAN-ONLY. Approve a waiting node.",
             {"node_run_id": _str("Node run id"), "reason": _str("Why"),
              "actor": _str("Who")}, ("node_run_id", "reason"),
             lambda c, a: c.approve_wait(a["node_run_id"], reason=a["reason"],
                                         actor=a.get("actor", "operator"))),
        Tool("reject_wait", "HUMAN-ONLY. Reject a waiting node.",
             {"node_run_id": _str("Node run id"), "reason": _str("Why"),
              "actor": _str("Who")}, ("node_run_id", "reason"),
             lambda c, a: c.reject_wait(a["node_run_id"], reason=a["reason"],
                                        actor=a.get("actor", "operator"))),
        Tool("resume_wait", "HUMAN-ONLY. Resume a node waiting on an external event.",
             {"node_run_id": _str("Node run id"), "event_type": _str("Event type"),
              "payload": _obj("Event payload")}, ("node_run_id", "event_type"),
             lambda c, a: c.resume_wait(a["node_run_id"], event_type=a["event_type"],
                                        payload=a.get("payload"))),
    ]


def _text(value: Any) -> dict:
    if isinstance(value, str):
        return {"type": "text", "text": value}
    return {"type": "text", "text": json.dumps(value, ensure_ascii=False, indent=2, default=str)}


def call_tool(tool: Tool, client: AnchorClient, arguments: dict,
              *, agent_can_approve: bool = False) -> dict:
    """Invoke one tool, mapping every failure to a structured MCP error result."""
    if tool.restricted and not agent_can_approve:
        return {"content": [{"type": "text", "text": json.dumps({
            "error": "human_only_operation", "tool": tool.name,
            "message": ("this operation is reserved for a human or operator; "
                        "ask the human to run it, or start the server with "
                        "ANCHOR_MCP_AGENT_CAN_APPROVE=1"),
        }, ensure_ascii=False)}], "isError": True}
    try:
        return {"content": [_text(tool.handler(client, arguments))], "isError": False}
    except AnchorApiError as exc:
        return {"content": [{"type": "text", "text": json.dumps(
            {"error": exc.code, **exc.as_dict(), "retryable": exc.retryable},
            ensure_ascii=False)}], "isError": True}
    except KeyError as exc:
        return {"content": [{"type": "text", "text": json.dumps(
            {"error": "missing_argument", "argument": str(exc), "tool": tool.name},
            ensure_ascii=False)}], "isError": True}
    except (TypeError, ValueError) as exc:
        return {"content": [{"type": "text", "text": json.dumps(
            {"error": "invalid_argument", "message": str(exc), "tool": tool.name},
            ensure_ascii=False)}], "isError": True}


def handle(message: dict, client: AnchorClient, *, agent_can_approve: bool = False,
           tools: list[Tool] | None = None) -> dict | None:
    """One JSON-RPC message in, one response out (None for notifications)."""
    method = message.get("method")
    identifier = message.get("id")
    params = message.get("params") or {}
    if method in ("notifications/initialized", "notifications/cancelled"):
        return None
    if method == "initialize":
        return {"jsonrpc": "2.0", "id": identifier, "result": {
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {"tools": {"listChanged": False}},
            "serverInfo": {"name": SERVER_NAME, "version": SERVER_VERSION},
        }}
    if method == "ping":
        return {"jsonrpc": "2.0", "id": identifier, "result": {}}
    if method == "tools/list":
        return {"jsonrpc": "2.0", "id": identifier,
                "result": {"tools": [tool.schema() for tool in tools or build_tools()]}}
    if method == "tools/call":
        name = params.get("name")
        table = {tool.name: tool for tool in (tools or build_tools())}
        tool = table.get(name) if isinstance(name, str) else None
        if tool is None:
            return {"jsonrpc": "2.0", "id": identifier, "error": {
                "code": -32602, "message": f"unknown tool: {name}",
                "data": {"available": sorted(table)}}}
        result = call_tool(tool, client, params.get("arguments") or {},
                           agent_can_approve=agent_can_approve)
        return {"jsonrpc": "2.0", "id": identifier, "result": result}
    return {"jsonrpc": "2.0", "id": identifier,
            "error": {"code": -32601, "message": f"method not found: {method}"}}


def serve(stdin=None, stdout=None, *, client: AnchorClient | None = None,
          agent_can_approve: bool = False) -> int:
    """Serve newline-delimited JSON-RPC until stdin closes."""
    stdin = stdin or sys.stdin
    stdout = stdout or sys.stdout
    tools = build_tools()
    owned = client is None
    client = client or AnchorClient()
    try:
        for line in stdin:
            line = line.strip()
            if not line:
                continue
            response: dict | None
            try:
                message = json.loads(line)
            except json.JSONDecodeError as exc:
                response = {"jsonrpc": "2.0", "id": None,
                            "error": {"code": -32700, "message": f"parse error: {exc}"}}
            else:
                response = handle(message, client, agent_can_approve=agent_can_approve,
                                  tools=tools)
            if response is not None:
                stdout.write(json.dumps(response, ensure_ascii=False, default=str) + "\n")
                stdout.flush()
    finally:
        if owned:
            client.close()
    return 0


def main() -> int:
    import os
    flag = os.environ.get("ANCHOR_MCP_AGENT_CAN_APPROVE", "").strip().lower()
    return serve(agent_can_approve=flag in ("1", "true", "yes"))


if __name__ == "__main__":
    raise SystemExit(main())
