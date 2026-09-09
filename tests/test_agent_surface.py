"""The agent surface: Graph IR reference, typed client and CLI.

These tests pin the contract an agent relies on: the IR reference is complete
enough to author a graph without guessing, the client drives a real run against
a live API process, and the CLI exposes the same operations with JSON output and
deterministic exit codes.
"""

import asyncio
import json
import threading
import time

import pytest
import uvicorn

from anchor.api.app import create_app
from anchor.runtime.dispatch import dispatch_pending
from anchor.runtime.receiver import DurableExecutionReceiver
from anchor.client import AnchorApiError, AnchorClient
from anchor.cli import main
from anchor.domain.graph import GraphDefinition
from anchor.domain.ir import describe_ir

TOKEN = "agent-surface-token-0123456789abcdef"

APPROVAL_GRAPH = {
    "graph_id": "agent-surface-gate",
    "name": "Agent surface gate",
    "nodes": [{"id": "gate", "type": "approval", "name": "Human gate"}],
    "edges": [],
}


def test_ir_reference_describes_every_node_type_and_a_valid_template():
    reference = describe_ir()
    assert reference["ir_version"] == "1"
    assert len(reference["node_types"]) == 12
    for node_type, contract in reference["node_types"].items():
        assert "notes" in contract
        if node_type in ("agent", "tool", "verifier", "subgraph"):
            assert contract["required_reference"]
        else:
            assert contract["required_reference"] is None
    GraphDefinition.model_validate(reference["template"])
    assert reference["condition"]["language"] == "JMESPath"
    assert reference["input_mapping"]["syntax"]


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
        """Accept admitted runs, mirroring the standalone receiver service."""
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


def test_client_drives_a_full_run_through_the_api(api_server):
    with AnchorClient(api_server, TOKEN) as client:
        installed = client.install(APPROVAL_GRAPH)
        version_id = installed["version"]["graph_version_id"]
        trigger = client.register_trigger(version_id)
        receipt = client.start_run(trigger["id"], objective="client smoke",
                                   idempotency_key="client-smoke-1")

        digest = client.wait_for_run(receipt["run_id"], timeout=3.0, interval=0.1)
        assert digest["timed_out"] is True
        assert digest["node_status_counts"] == {"waiting_approval": 1}
        waiting = client.list_waits()
        assert len(waiting) == 1

        client.approve_wait(waiting[0]["node_run"]["id"], reason="approved by test")
        digest = client.wait_for_run(receipt["run_id"], timeout=5.0, interval=0.1)
        assert digest["terminal"] is True and digest["status"] == "completed"
        assert digest["node_status_counts"] == {"completed": 1}

        events = list(client.iter_events(receipt["run_id"]))
        assert [event["sequence"] for event in events] == sorted(
            event["sequence"] for event in events)
        assert any(event["event_type"].startswith("run.") for event in events)


def test_client_reports_structured_errors(api_server):
    with AnchorClient(api_server, TOKEN) as client:
        with pytest.raises(AnchorApiError) as failure:
            client.get_run("00000000-0000-0000-0000-000000000000")
        assert failure.value.status == 404
        assert failure.value.code


def test_cli_prints_json_and_uses_deterministic_exit_codes(api_server, monkeypatch, capsys):
    monkeypatch.setenv("ANCHOR_API_TOKEN", TOKEN)
    assert main(["--api-url", api_server, "ir"]) == 0
    payload = json.loads(capsys.readouterr().out)
    assert payload["ir_version"] == "1"

    assert main(["--api-url", api_server, "graph", "install", "--file", "/does/not/exist.json"]) == 2
    error = json.loads(capsys.readouterr().err)
    assert error["error"]["code"] == "client_error"

    assert main(["--api-url", api_server, "run", "show",
                 "00000000-0000-0000-0000-000000000000"]) == 1
    error = json.loads(capsys.readouterr().err)
    assert error["error"]["status"] == 404


def test_cli_installs_starts_and_observes_a_run(api_server, monkeypatch, capsys, tmp_path):
    monkeypatch.setenv("ANCHOR_API_TOKEN", TOKEN)
    path = tmp_path / "graph.json"
    path.write_text(json.dumps(APPROVAL_GRAPH), encoding="utf-8")

    assert main(["--api-url", api_server, "graph", "validate", "--file", str(path)]) == 0
    result = json.loads(capsys.readouterr().out)
    assert result["structural"]["valid"] and result["capabilities"]["valid"]

    assert main(["--api-url", api_server, "graph", "install", "--file", str(path)]) == 0
    version_id = json.loads(capsys.readouterr().out)["version"]["graph_version_id"]

    assert main(["--api-url", api_server, "trigger", "add", "--version", version_id]) == 0
    trigger_id = json.loads(capsys.readouterr().out)["id"]

    assert main(["--api-url", api_server, "run", "start", "--trigger", trigger_id,
                 "--objective", "cli run", "--idempotency-key", "cli-run-1"]) == 0
    run_id = json.loads(capsys.readouterr().out)["run_id"]

    assert main(["--api-url", api_server, "run", "watch", run_id,
                 "--timeout", "3", "--interval", "0.1"]) == 0
    digest = json.loads(capsys.readouterr().out)
    assert digest["node_status_counts"] == {"waiting_approval": 1}
