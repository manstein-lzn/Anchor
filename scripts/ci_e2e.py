#!/usr/bin/env python
"""Provider-free end-to-end check of the chain an agent host drives.

Unit tests cover the parts. This covers the seam between them, which is where integration
defects live: a graph authored over MCP, admitted, executed by a real worker, judged by a
verifier, held at a human gate, approved, and observed — with **no provider reachable**, so
the only model answers are the ones in a checked-in recording.

What it protects against is the failure mode where every part passes and the chain does not,
and where the only way to find out is to run it live and watch. It also makes a failure
point at the node and call sequence that diverged rather than at "the run failed".

Everything is built in a temporary directory: a fresh database, artifact root, token and
runtime profile. The fixture under `tests/fixtures/replay/` is read and hashed before and
after, so a check that mutated it would fail rather than silently rewrite the thing it is
measuring against.

    .venv/bin/python scripts/ci_e2e.py
"""

from __future__ import annotations

import asyncio
import hashlib
import json
import os
import pathlib
import shutil
import socket
import subprocess
import sys
import tempfile
import time
from typing import Any
from uuid import uuid4

ROOT = pathlib.Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "src"))

FIXTURE_DIR = ROOT / "tests" / "fixtures" / "replay"
FIXTURE = FIXTURE_DIR / "planner.json"
MCP = ROOT / ".venv" / "bin" / "anchor-mcp"
TOKEN = "ci-e2e-token-not-a-real-secret-at-all-32-plus"

#: The node the fixture answers for. The replay substitutes by call ordinal and *checks* the
#: node, so a graph that asks in a different order fails here rather than mispairing.
PLAN_NODE = "plan"

GRAPH = {
    "graph_id": "ci-e2e",
    "name": "CI end to end",
    "entry_node_id": "start",
    "nodes": [
        {"id": "start", "type": "artifact", "name": "Start"},
        {"id": PLAN_NODE, "type": "agent", "name": "Plan", "agent_ref": "agents.academic.planner"},
        {"id": "check", "type": "verifier", "name": "Check", "verifier_ref": "verifiers.plan_is_json"},
        {"id": "approve", "type": "approval", "name": "Approve"},
        {"id": "done", "type": "artifact", "name": "Done"},
    ],
    "edges": [
        {"source": "start", "target": PLAN_NODE},
        {"source": PLAN_NODE, "target": "check"},
        {"source": "check", "target": "approve"},
        {"source": "approve", "target": "done"},
    ],
}


def digest(path: pathlib.Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def free_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


def runtime_profile(source: dict[str, Any]) -> dict[str, Any]:
    """The repo's profile, plus a deterministic verifier that needs no model.

    Added here rather than to the checked-in profile because it exists to make this check
    exercise the verifier seam without a provider, not because the deployment wants it.
    """
    return {**source, "verifiers": [{
        "ref": "verifiers.plan_is_json",
        "version": "v1",
        "adapter": "deterministic",
        # The verifier sees {task, context, artifacts}, so the expression addresses the
        # evidence it was given rather than a bare `output`.
        "expression": "length(artifacts) > `0`",
    }]}


class Mcp:
    """The packaged server, driven the way an agent host drives it."""

    def __init__(self, api_url: str) -> None:
        self.process = subprocess.Popen(
            [str(MCP)], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=subprocess.PIPE, text=True, bufsize=1,
            env={**os.environ, "ANCHOR_API_URL": api_url, "ANCHOR_API_TOKEN": TOKEN})
        self.next_id = 0

    def send(self, method: str, params: dict | None = None) -> dict:
        self.next_id += 1
        message: dict = {"jsonrpc": "2.0", "id": self.next_id, "method": method}
        if params is not None:
            message["params"] = params
        self._write(message)
        return self._read(method)

    def notify(self, method: str, params: dict | None = None) -> None:
        """Write a notification and do not wait for a reply.

        JSON-RPC notifications have no id and get no response, so reading one blocks forever on
        a server that is behaving correctly — which reads as a hung server rather than as a
        client waiting for something that was never coming.
        """
        message: dict = {"jsonrpc": "2.0", "method": method}
        if params is not None:
            message["params"] = params
        self._write(message)

    def _write(self, message: dict) -> None:
        assert self.process.stdin
        self.process.stdin.write(json.dumps(message) + "\n")
        self.process.stdin.flush()

    def _read(self, method: str) -> dict:
        assert self.process.stdout
        line = self.process.stdout.readline()
        if not line:
            stderr = self.process.stderr.read() if self.process.stderr else ""
            raise RuntimeError(f"the MCP server closed stdout after {method}: {stderr[:400]}")
        return json.loads(line)

    def tool(self, name: str, arguments: dict | None = None) -> Any:
        response = self.send("tools/call", {"name": name, "arguments": arguments or {}})
        if "error" in response:
            raise RuntimeError(f"{name}: protocol error {response['error']}")
        result = response["result"]
        text = result["content"][0]["text"]
        payload = json.loads(text) if text.strip().startswith(("{", "[")) else text
        if result.get("isError"):
            raise RuntimeError(f"{name} refused: {payload}")
        return payload

    def close(self) -> None:
        if self.process.stdin:
            self.process.stdin.close()
        self.process.wait(timeout=10)


def wait_for(probe, *, timeout: float, interval: float = 0.2, what: str):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = probe()
        if value:
            return value
        time.sleep(interval)
    raise SystemExit(f"timed out after {timeout:.0f}s waiting for {what}")


def main() -> int:
    from anchor.runtime.config import load_runtime_config

    if not MCP.exists():
        print(f"the MCP entry point is missing at {MCP}; run `pip install -e .`")
        return 2
    before = digest(FIXTURE)
    fixture = json.loads(FIXTURE.read_text(encoding="utf-8"))
    fixture_run = fixture["run_id"]
    report: dict[str, Any] = {"fixture": {"path": str(FIXTURE.relative_to(ROOT)),
                                          "run_id": fixture_run,
                                          "records_node": fixture["node_id"]},
                              "steps": []}

    work = pathlib.Path(tempfile.mkdtemp(prefix="anchor-ci-e2e-"))
    api: subprocess.Popen | None = None
    try:
        profile = runtime_profile(json.loads(
            (ROOT / ".local" / "runtime.json").read_text(encoding="utf-8")))
        profile_path = work / "runtime.json"
        profile_path.write_text(json.dumps(profile), encoding="utf-8")
        database_url = f"sqlite:///{work / 'ci.sqlite'}"
        token_file = work / "api-token"
        token_file.write_text(TOKEN, encoding="utf-8")
        artifacts_root = work / "artifacts"

        from alembic import command
        from alembic.config import Config as AlembicConfig

        from anchor.runtime.artifacts import LocalArtifactStore
        from anchor.state.relational import RelationalStateStore

        # Migrate explicitly so a schema problem fails at setup rather than halfway through a
        # run, where it would look like a chain defect.
        schema = AlembicConfig(str(ROOT / "alembic.ini"))
        schema.set_main_option("script_location", str(ROOT / "migrations"))
        schema.attributes["database_url"] = database_url
        command.upgrade(schema, "head")
        store = RelationalStateStore(database_url)
        artifacts = LocalArtifactStore(artifacts_root)

        # Register the fixture as what its run recorded. Content addressing means the ref is
        # derived from the bytes, so the fixture's own ref is reproducible, and writing a
        # `model.call` event means the ordinary replay path finds it — rather than this check
        # having a private way to load recordings that the runtime does not use.
        from uuid import UUID as _UUID

        ref = artifacts.put_text(FIXTURE.read_text(encoding="utf-8"))
        store.append_event(stream_id=_UUID(fixture_run), event_type="model.call",
                           payload={"node_id": fixture["node_id"],
                                    "node_run_id": fixture["node_run_id"],
                                    "attempt": fixture["attempt"],
                                    "sequence": fixture["sequence"],
                                    "recording_ref": ref},
                           idempotency_key=f"fixture:{fixture['node_run_id']}")
        from anchor.runtime.model_replay import ReplayPlan

        recorded = ReplayPlan(artifacts, store=store).ordered(fixture_run)
        if not recorded:
            raise SystemExit(f"the fixture at {FIXTURE} yielded no recordings")
        report["fixture"]["ref"] = ref
        report["fixture"]["calls"] = len(recorded)
        report["steps"].append("fixture registered")

        print("── 1. the API, in a temporary environment ──")
        port = free_port()
        api_url = f"http://127.0.0.1:{port}"
        api = subprocess.Popen(
            [sys.executable, "-m", "anchor.api", "--port", str(port),
             "--token-file", str(token_file)],
            cwd=ROOT, env={**os.environ, "ANCHOR_DATABASE_URL": database_url,
                           "ANCHOR_ARTIFACT_ROOT": str(artifacts_root),
                           "ANCHOR_RUNTIME_CONFIG": str(profile_path),
                           "ANCHOR_API_TOKEN": TOKEN},
            stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
        from anchor.client import AnchorClient

        def ready() -> bool:
            try:
                with AnchorClient(api_url, token_file=str(token_file)) as client:
                    return client.health().get("status") == "ready"
            except Exception:  # noqa: BLE001 - still starting
                return False

        wait_for(ready, timeout=60, interval=0.3, what="the API to become ready")
        print(f"   ready on {api_url}")
        report["steps"].append("api ready")

        print("── 2. author over MCP ──")
        mcp = Mcp(api_url)
        try:
            mcp.send("initialize", {"protocolVersion": "2024-11-05",
                                    "capabilities": {},
                                    "clientInfo": {"name": "ci-e2e", "version": "1"}})
            mcp.notify("notifications/initialized")
            tools = {item["name"] for item in mcp.send("tools/list")["result"]["tools"]}
            for expected in ("install", "start_run", "run_events", "approve_wait"):
                if expected not in tools:
                    raise SystemExit(f"MCP does not expose {expected}: {sorted(tools)}")
            authored = mcp.tool("install", {"definition": GRAPH, "publish": True})
            version_id = authored["version"]["graph_version_id"]
            print(f"   published {version_id}")
            report["steps"].append("authored over MCP")
            report["graph_version_id"] = version_id
        finally:
            mcp.close()

        print("── 3. run it: control, agent and verifier workers, replay only ──")
        from anchor.runtime.academic import register_academic_behaviors
        from anchor.runtime.behaviors import BehaviorRegistry
        from anchor.runtime.capabilities import CapabilityRegistry
        from anchor.runtime.control_worker import ControlNodeWorker
        from anchor.runtime.dispatch import dispatch_pending
        from anchor.runtime.model_gateway import build_model_gateway
        from anchor.runtime.receiver import DurableExecutionReceiver
        from anchor.runtime.model_recording import ModelRecorder, RecordingMode
        from anchor.runtime.secrets import (ChainedSecretProvider, EnvironmentSecretProvider,
                                            JsonFileSecretProvider)
        from anchor.runtime.sinks import ArtifactCheckpointSink
        from anchor.runtime.sinks import VerificationCheckpointSink
        from anchor.runtime.verifier import VerifierNodeWorker

        config = load_runtime_config(str(profile_path))
        registry = CapabilityRegistry(models=config.models, agents=config.agents,
                                      tools=config.tools, verifiers=config.verifiers)
        providers: list[Any] = [EnvironmentSecretProvider(),
                                JsonFileSecretProvider(config.secret_file)]
        recorder = ModelRecorder(artifacts, mode=RecordingMode.REPLAY, store=store,
                                 replay_of=fixture_run)
        gateways = {item.ref: build_model_gateway(item, ChainedSecretProvider(*providers),
                                                  recorder=recorder)
                    for item in config.models}

        def is_replay(model: Any) -> bool:
            from anchor.runtime.model_replay import ReplayModel

            return isinstance(getattr(model, "_model", None), ReplayModel)

        if not all(is_replay(gateway) for gateway in gateways.values()):
            raise SystemExit("a gateway is not in replay mode; the check would call a provider")
        behaviors = BehaviorRegistry()
        register_academic_behaviors(behaviors)
        control = ControlNodeWorker(store, artifacts,
                                    ArtifactCheckpointSink(store, artifacts, "control"),
                                    behaviors=behaviors)
        from anchor.runtime.worker import AgentNodeWorker

        worker = AgentNodeWorker(store, registry, gateways,
                                 ArtifactCheckpointSink(store, artifacts, "agent"),
                                 behaviors=behaviors)
        verifier = VerifierNodeWorker(store, registry, gateways, artifacts,
                                      VerificationCheckpointSink(store, "verifier"))

        with AnchorClient(api_url, token_file=str(token_file)) as client:
            trigger = client.register_trigger(version_id)
            run_id = client.start_run(
                trigger["id"], objective="Plan a short review on cost models.",
                idempotency_key=f"ci-{uuid4().hex}")["run_id"]
            print("   run:", run_id)
            report["run_id"] = run_id

            task_prompt = ("Task objective:\n"
                           "Plan a short literature review on cost models.\n\n"
                           "Execute graph node: Plan\n\n"
                           "Durable input snapshot:\n{}")

            approved: list[str] = []

            async def drive() -> None:
                # One event loop: the gateway's HTTP client binds to the loop it is first used
                # in, and a second asyncio.run per attempt fails in a way that reads as a
                # provider fault. scripts/node_harness.py records the same lesson.
                #
                # The gate is passed from inside the loop rather than after it: an approval node
                # is not a terminal status, so a driver that waits for one before looking at the
                # gate waits forever on a run that is behaving correctly.
                for _ in range(1200):
                    # Without this the run never leaves `created`: admitting a run writes to
                    # the outbox and the receiver is what dispatches it.
                    await dispatch_pending(store, DurableExecutionReceiver(store))
                    await control.execute_once(worker_id="control")
                    lease = store.claim_ready_agent_node("agent", uuid4())
                    if lease is not None:
                        await worker.execute_claimed_once(
                            worker_id="agent", agent_ref="agents.academic.planner",
                            lease=lease, prompt=task_prompt, heartbeat_interval=0.01,
                            input_snapshot={})
                    await verifier.execute_once(worker_id="verifier")
                    for wait in client.list_waits():
                        node_run = wait.get("node_run") or {}
                        node_run_id = node_run.get("id")
                        if node_run_id is None or node_run_id in approved:
                            continue
                        approved.append(node_run_id)
                        client.approve_wait(node_run_id, reason="ci-e2e")
                        print(f"   approved the gate on {node_run.get('node_id')}")
                    if store.get_run(run_id).status.value in ("completed", "failed", "cancelled"):
                        return
                    await asyncio.sleep(0.05)

            asyncio.run(drive())
            state = client.get_run(run_id)["status"]
            print("   status:", state)
            report["approved"] = approved
            report["steps"].append(f"run reached {state}")

            print("── 4. observe over MCP ──")
            waits = client.list_waits()
            if state == "failed":
                nodes = client.run_nodes(run_id)
                events = client.run_events(run_id, limit=200)
                report["nodes"] = [(n["node_id"], n["status"], n.get("error_code"))
                                   for n in nodes]
                report["events"] = [event["event_type"] for event in events]
                print(json.dumps(report, ensure_ascii=False, indent=2)[:3000])
                print("\nFAILED: the run did not complete; the node and event path is above")
                return 1
            report["waits"] = waits

            mcp = Mcp(api_url)
            try:
                mcp.send("initialize", {"protocolVersion": "2024-11-05", "capabilities": {},
                                        "clientInfo": {"name": "ci-e2e", "version": "1"}})
                mcp.notify("notifications/initialized")
                observed = mcp.tool("run_events", {"run_id": run_id, "limit": 50})
                report["observed_over_mcp"] = str(observed)[:200]
            finally:
                mcp.close()

            final = client.get_run(run_id)["status"]
            nodes = client.run_nodes(run_id)
            events = [event["event_type"] for event in client.run_events(run_id, limit=200)]
            report["final_status"] = final
            report["nodes"] = [(n["node_id"], n["status"]) for n in nodes]
            report["events"] = events

        recorder_report = recorder.mode.value
        report["recording_mode"] = recorder_report
        report["fixture_unchanged"] = digest(FIXTURE) == before

        print("── 5. what the chain did ──")
        print("   nodes:", report["nodes"])
        print("   events:", " ".join(events)[:400])
        failures: list[str] = []
        if final != "completed":
            failures.append(f"the run ended {final}")
        if not report["fixture_unchanged"]:
            failures.append("the fixture was modified")
        expected_nodes = {"start", PLAN_NODE, "check", "approve", "done"}
        if {name for name, _ in report["nodes"]} != expected_nodes:
            failures.append(f"the run did not visit every node: {report['nodes']}")
        if "model.call_replayed" not in events:
            failures.append("no call was served from the recording, so nothing was replayed")
        if "model.call" in events:
            failures.append("a live model call was recorded, so a provider was reached")
        report["failures"] = failures

        out = work / "report.json"
        out.write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8")
        print("written:", out)
        if failures:
            print("\nFAILED:", "; ".join(failures))
            return 1
        print("\nPASSED")
        return 0
    finally:
        if api is not None and api.poll() is None:
            api.terminate()
            api.wait(timeout=10)
        shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    raise SystemExit(main())
