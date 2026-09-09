"""The MCP surface: an agent drives Anchor through the same API and guards.

These tests pin the contract: the tool list is discoverable and complete, a tool
call reaches the real API, failures come back as structured error results, and
the human-only operations are refused by default. The MCP server is an adapter,
never a backdoor — so `approve_wait` must fail here exactly because it would
make the approval gate theatre.
"""

import asyncio
import io
import json
import threading
import time
from uuid import uuid4

import pytest
import uvicorn

from anchor.api.app import create_app
from anchor.client import AnchorClient
from anchor.mcp import HUMAN_ONLY, OPERATOR_ONLY, build_tools, handle, serve
from anchor.runtime.dispatch import dispatch_pending
from anchor.runtime.receiver import DurableExecutionReceiver

TOKEN = "mcp-surface-token-0123456789abcdef"

GATE_GRAPH = {
    "graph_id": "mcp-gate",
    "name": "MCP gate",
    "nodes": [{"id": "gate", "type": "approval", "name": "Human gate"}],
    "edges": [],
}


@pytest.fixture
def api_server(store):
    app = create_app(store, TOKEN)
    config = uvicorn.Config(app, host="127.0.0.1", port=0, log_level="warning")
    server = uvicorn.Server(config)
    thread = threading.Thread(target=server.run, daemon=True)
    thread.start()
    for _ in range(200):
        if server.started:
            break
        time.sleep(0.05)
    assert server.started, "uvicorn did not start"
    port = server.servers[0].sockets[0].getsockname()[1]

    stop = threading.Event()

    def pump():
        while not stop.is_set():
            try:
                asyncio.run(dispatch_pending(store, DurableExecutionReceiver(store)))
            except Exception:
                pass
            stop.wait(0.05)

    pump_thread = threading.Thread(target=pump, daemon=True)
    pump_thread.start()
    try:
        yield f"http://127.0.0.1:{port}"
    finally:
        stop.set()
        pump_thread.join(timeout=2)
        server.should_exit = True
        thread.join(timeout=5)


@pytest.fixture
def client(api_server):
    with AnchorClient(api_server, TOKEN) as opened:
        yield opened


def call(client, name, arguments=None, **kwargs):
    return handle({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                   "params": {"name": name, "arguments": arguments or {}}},
                  client, **kwargs)


def payload(response):
    return json.loads(response["result"]["content"][0]["text"])


def test_tool_list_is_complete_and_every_schema_is_closed():
    tools = build_tools()
    names = {tool.name for tool in tools}
    assert len(names) == len(tools), "duplicate tool names"
    assert HUMAN_ONLY | OPERATOR_ONLY <= names
    assert {"install", "start_run", "run_digest", "wait_for_run", "ir"} <= names
    for tool in tools:
        schema = tool.schema()["inputSchema"]
        assert schema["additionalProperties"] is False
        assert schema["required"] == list(tool.required)
        assert set(tool.required) <= set(schema["properties"])


def test_initialize_advertises_tool_capability(client):
    response = handle({"jsonrpc": "2.0", "id": 7, "method": "initialize", "params": {}}, client)
    assert response["id"] == 7
    assert response["result"]["capabilities"]["tools"] == {"listChanged": False}
    assert response["result"]["serverInfo"]["name"] == "anchor"


def test_notifications_get_no_response(client):
    assert handle({"jsonrpc": "2.0", "method": "notifications/initialized"}, client) is None


def test_tool_call_reaches_the_real_api(client):
    response = call(client, "health")
    assert response["result"]["isError"] is False
    assert payload(response)["status"] == "ready"

    installed = call(client, "install", {"definition": GATE_GRAPH})
    assert installed["result"]["isError"] is False
    assert payload(installed)["version"]["graph_version_id"]

    missing = call(client, "run_nodes", {"run_id": str(uuid4())})
    assert missing["result"]["isError"] is True
    assert payload(missing)["status"] == 404


def test_human_only_operations_are_refused_by_default(client):
    for name in sorted(HUMAN_ONLY | OPERATOR_ONLY):
        response = call(client, name, {"node_run_id": "x", "reason": "x"})
        assert response["result"]["isError"] is True
        assert payload(response)["error"] == "human_only_operation"
        assert payload(response)["tool"] == name


def test_human_only_operations_work_when_the_operator_opts_in(client):
    installed = call(client, "install", {"definition": GATE_GRAPH})
    version_id = payload(installed)["version"]["graph_version_id"]
    trigger = call(client, "register_trigger", {"graph_version_id": version_id})
    started = call(client, "start_run", {"trigger_id": payload(trigger)["id"],
                                         "objective": "opt in", "idempotency_key": "mcp-opt-in"})
    run_id = payload(started)["run_id"]

    for _ in range(200):
        digest = payload(call(client, "run_digest", {"run_id": run_id}))
        if digest["node_status_counts"] == {"waiting_approval": 1}:
            break
        time.sleep(0.05)
    node_run_id = digest["waiting"][0]["node_run_id"]

    refused = call(client, "approve_wait", {"node_run_id": node_run_id, "reason": "no"})
    assert refused["result"]["isError"] is True

    allowed = call(client, "approve_wait", {"node_run_id": node_run_id, "reason": "yes"},
                   agent_can_approve=True)
    assert allowed["result"]["isError"] is False
    final = payload(call(client, "run_digest", {"run_id": run_id}))
    assert final["node_status_counts"] == {"completed": 1}, final


def test_unknown_tool_is_a_protocol_error(client):
    response = call(client, "not_a_tool")
    assert response["error"]["code"] == -32602
    assert "install" in response["error"]["data"]["available"]


def test_missing_argument_is_a_structured_error(client):
    response = call(client, "get_version", {})
    assert response["result"]["isError"] is True
    assert payload(response)["error"] == "missing_argument"


def test_serve_reads_newline_delimited_jsonrpc(client):
    request = "\n".join([
        json.dumps({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
        json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json.dumps({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}),
        json.dumps({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
                    "params": {"name": "health", "arguments": {}}}),
        "{not json",
    ]) + "\n"
    out = io.StringIO()
    assert serve(io.StringIO(request), out, client=client) == 0
    lines = [json.loads(line) for line in out.getvalue().splitlines()]
    assert [line["id"] for line in lines] == [1, 2, 3, None]
    assert len(lines[1]["result"]["tools"]) == len(build_tools())
    assert lines[2]["result"]["isError"] is False
    assert lines[3]["error"]["code"] == -32700
