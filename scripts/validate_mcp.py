#!/usr/bin/env python3
"""End-to-end MCP validation: an MCP client authors, runs and observes Anchor.

Spawns the real `anchor-mcp` process and speaks JSON-RPC to it over stdio, the
same way an agent host does. The graph is a single control node, so the run
completes deterministically with no model call and no cost.

What this proves that unit tests cannot:
  - the packaged entry point starts and speaks the protocol,
  - a tool call reaches the live API through the real client and token,
  - a run admitted by an agent actually reaches a terminal state,
  - the human-only gate refuses even when the agent asks directly.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
MCP = ROOT / ".venv" / "bin" / "anchor-mcp"
TIMEOUT = float(os.environ.get("ANCHOR_VALIDATE_TIMEOUT", "180"))

GRAPH = {
    "graph_id": "mcp-validation",
    "name": "MCP validation",
    "nodes": [{"id": "fan", "type": "parallel", "name": "Fan out"}],
    "edges": [],
}

failures: list[str] = []


def check(name: str, condition: object, detail: object = "") -> bool:
    ok = bool(condition)
    print(f"  {'PASS' if ok else 'FAIL'}  {name}{'' if ok else ' — ' + str(detail)}")
    if not ok:
        failures.append(name)
    return ok


class McpClient:
    """Minimal stdio JSON-RPC client: what an agent host does, nothing more."""

    def __init__(self) -> None:
        self.process = subprocess.Popen(
            [str(MCP)], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=subprocess.PIPE, text=True, bufsize=1,
            env={**os.environ, "ANCHOR_API_URL": os.environ.get(
                "ANCHOR_API_URL", "http://127.0.0.1:8090")})
        self.next_id = 0

    def send(self, method: str, params: dict | None = None) -> dict | None:
        self.next_id += 1
        message: dict = {"jsonrpc": "2.0", "id": self.next_id, "method": method}
        if params is not None:
            message["params"] = params
        assert self.process.stdin and self.process.stdout
        self.process.stdin.write(json.dumps(message) + "\n")
        self.process.stdin.flush()
        line = self.process.stdout.readline()
        if not line:
            raise RuntimeError(f"server closed stdout after {method}: "
                               f"{self.process.stderr.read() if self.process.stderr else ''}")
        return json.loads(line)

    def tool(self, name: str, arguments: dict | None = None) -> dict:
        """Call a tool, returning the parsed payload; raise on isError."""
        response = self.send("tools/call", {"name": name, "arguments": arguments or {}})
        if "error" in response:
            raise RuntimeError(f"{name}: protocol error {response['error']}")
        result = response["result"]
        text = result["content"][0]["text"]
        payload = json.loads(text) if text.strip().startswith(("{", "[")) else text
        if result.get("isError"):
            raise RuntimeError(f"{name} failed: {payload}")
        return payload

    def close(self) -> int:
        if self.process.stdin:
            self.process.stdin.close()
        return self.process.wait(timeout=10)


def main() -> int:
    if not MCP.exists():
        print(f"FAIL: {MCP} missing; run `pip install -e .`", file=sys.stderr)
        return 1

    client = McpClient()
    try:
        print("== 1. protocol handshake")
        init = client.send("initialize", {"protocolVersion": "2024-11-05",
                                          "capabilities": {}, "clientInfo": {
                                              "name": "validate-mcp", "version": "1"}})
        check("initialize advertises the tools capability",
              init["result"]["capabilities"]["tools"] == {"listChanged": False}, init)
        client.process.stdin.write(json.dumps(
            {"jsonrpc": "2.0", "method": "notifications/initialized"}) + "\n")
        client.process.stdin.flush()

        listed = client.send("tools/list", {})["result"]["tools"]
        check("tools/list returns a non-trivial surface", len(listed) >= 30, len(listed))
        check("every tool schema is closed",
              all(tool["inputSchema"]["additionalProperties"] is False for tool in listed))
        check("human-only tools are advertised but gated",
              "approve_wait" in {tool["name"] for tool in listed})

        print("== 2. author through MCP")
        check("health reaches the live API", client.tool("health")["status"] == "ready")
        installed = client.tool("install", {"definition": GRAPH})
        version_id = installed["version"]["graph_version_id"]
        check("install published a version", bool(version_id), installed)
        trigger = client.tool("register_trigger", {"graph_version_id": version_id})
        check("trigger registered", bool(trigger["id"]), trigger)

        print("== 3. execute and observe through MCP")
        receipt = client.tool("start_run", {"trigger_id": trigger["id"],
                                            "objective": "MCP end-to-end",
                                            "idempotency_key": f"mcp-{int(time.time())}"})
        run_id = receipt["run_id"]
        check("run admitted", bool(run_id), receipt)

        digest = client.tool("wait_for_run", {"run_id": run_id, "timeout": TIMEOUT,
                                              "interval": 1.0})
        check("run reached a terminal state", digest["terminal"] is True, digest)
        check("run completed", digest["status"] == "completed", digest)

        events = client.tool("run_events", {"run_id": run_id, "after": 0, "limit": 100})
        check("events are readable and ordered",
              bool(events) and events == sorted(events, key=lambda item: item["sequence"]),
              len(events))
        nodes = client.tool("run_nodes", {"run_id": run_id})
        check("the control node completed",
              [node["status"] for node in nodes] == ["completed"], nodes)
        check("a second digest is consistent",
              client.tool("run_digest", {"run_id": run_id})["node_status_counts"]
              == {"completed": 1})

        print("== 4. the gate is real")
        refused = client.send("tools/call", {"name": "approve_wait",
                                             "arguments": {"node_run_id": "x", "reason": "x"}})
        check("approve_wait is refused for the agent",
              refused["result"]["isError"] is True
              and json.loads(refused["result"]["content"][0]["text"])["error"]
              == "human_only_operation", refused)
        unknown = client.send("tools/call", {"name": "no_such_tool", "arguments": {}})
        check("unknown tool is a protocol error", unknown["error"]["code"] == -32602, unknown)
        missing = client.send("tools/call", {"name": "get_version", "arguments": {}})
        check("missing argument is structured",
              json.loads(missing["result"]["content"][0]["text"])["error"] == "missing_argument",
              missing)

        print("== 5. clean shutdown")
        check("server exits 0 when stdin closes", client.close() == 0)
    finally:
        if client.process.poll() is None:
            client.process.kill()

    if failures:
        print(f"\nMCP VALIDATION FAILED: {', '.join(failures)}")
        return 1
    print("\nMCP VALIDATION PASSED")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
