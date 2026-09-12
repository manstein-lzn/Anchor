#!/usr/bin/env python
"""Acceptance for P0.2: a branch failure ends everything the run will never reach.

The defect was measured first: failed runs held 76 nodes in `pending` with no terminal
state and nothing recording why. From outside that is indistinguishable from a run still
waiting for a worker.

Against the running services, on a real fork:

  1. publish `start -> a, b` with `a -> tail -> end` and `b -> end`, and admit a run
  2. wait until both branches hold a live lease, so one is in flight and one is not
  3. fail one branch through the public lease-failure endpoint
  4. assert the run failed, that no node is left non-terminal, that every abandoned node
     records a reason, and that no lease survives
  5. restart the supervisor and assert nothing about the run changed

Uses `anchor.client` for everything the agent surface exposes. Lease observation and failure
have no client method yet; that gap is recorded in the output rather than worked around
silently.

    ANCHOR_DATABASE_URL=... .venv/bin/python scripts/validate_failure_fanout.py
"""

from __future__ import annotations

import json
import os
import pathlib
import subprocess
import sys
import time
import urllib.error
import urllib.request
from typing import Any

ROOT = pathlib.Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "src"))

from anchor.client import AnchorClient  # noqa: E402

API = os.environ.get("ANCHOR_API_URL", "http://127.0.0.1:8090")
TOKEN = (ROOT / ".local" / "api-token").read_text().strip()
AGENT = os.environ.get("ANCHOR_ACCEPTANCE_AGENT", "agents.academic.planner")
TERMINAL = ("completed", "failed", "cancelled", "skipped")

FORK = {
    "graph_id": "fanout-acceptance",
    "name": "Fan-out acceptance",
    "entry_node_id": "start",
    "nodes": [
        {"id": "start", "type": "artifact", "name": "Start"},
        {"id": "a", "type": "agent", "name": "A", "agent_ref": AGENT},
        {"id": "b", "type": "agent", "name": "B", "agent_ref": AGENT},
        {"id": "tail", "type": "agent", "name": "Tail", "agent_ref": AGENT},
        {"id": "end", "type": "artifact", "name": "End"},
    ],
    "edges": [
        {"source": "start", "target": "a"},
        {"source": "start", "target": "b"},
        {"source": "a", "target": "tail"},
        {"source": "tail", "target": "end"},
        {"source": "b", "target": "end"},
    ],
}


def raw(method: str, path: str, body: Any = None) -> Any:
    request = urllib.request.Request(
        API + path, method=method,
        data=None if body is None else json.dumps(body).encode(),
        headers={"Authorization": f"Bearer {TOKEN}", "Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            return json.loads(response.read() or "null")
    except urllib.error.HTTPError as exc:
        return {"__status__": exc.code, "body": exc.read().decode()[:300]}


def systemctl(*args: str) -> str:
    result = subprocess.run(["systemctl", "--user", *args], capture_output=True, text=True)
    return (result.stdout + result.stderr).strip()


def leases(run_id: str, *, stale_after: float = 600) -> list[dict]:
    entries = raw("GET", f"/api/leases/active?stale_after={stale_after:.0f}&run_id={run_id}") or []
    return [entry for entry in entries
            if str((entry.get("lease") or {}).get("run_id")) == run_id]


def wait_for(predicate, *, timeout: float, interval: float = 0.05, what: str) -> Any:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(interval)
    raise SystemExit(f"timed out after {timeout:.0f}s waiting for {what}")


def states(client: AnchorClient, run_id: str) -> dict[str, str]:
    return {item["node_id"]: item["status"] for item in client.run_nodes(run_id)}


def main() -> int:
    stamp = int(time.time())
    evidence: dict[str, Any] = {"api": API, "agent": AGENT}
    with AnchorClient(API, token_file=str(ROOT / ".local" / "api-token")) as client:
        print("── 0. readiness ──")
        ready = client.health()
        print("  ", json.dumps(ready, ensure_ascii=False))
        evidence["readiness"] = ready
        if not all(ready.get(key) for key in ("execution_connected", "worker_connected",
                                              "control_worker_connected",
                                              "verifier_worker_connected")):
            print("  not every role is connected; the acceptance would be meaningless")
            return 1

        print("── 1. publish the fork and admit a run ──")
        definition = {**FORK, "graph_id": f"{FORK['graph_id']}-{stamp}"}
        installed = client.install(definition)
        version_id = installed["version"]["graph_version_id"]
        trigger = client.register_trigger(version_id)
        run_id = client.start_run(trigger["id"],
                                  objective="Plan a short literature review on cost models.",
                                  idempotency_key=f"fanout-{stamp}")["run_id"]
        print("  run:", run_id)
        evidence["run_id"] = run_id

        print("── 2. wait until a branch holds a live lease ──")
        # One worker process claims one node at a time, so the branches are serialised: the
        # second is `ready` rather than in flight. That is still one of the four timings the
        # plan asks for, and the in-flight case is covered by the unit tests, which can hold
        # two leases at once. Both cannot be in flight here without a second worker process.
        held = wait_for(
            lambda: [entry for entry in leases(run_id)
                     if entry.get("node_type") == "agent"],
            timeout=240, interval=0.05, what="an agent branch to be claimed")
        victim_node = held[0]["lease"]["node_id"]
        claim_id = held[0]["lease"]["claim_id"]
        print(f"  in flight: {victim_node}; the sibling is not yet claimed")
        evidence["leases_before"] = held
        evidence["victim"] = victim_node

        print("── 3. fail that branch through the public endpoint ──")
        failed = raw("POST", f"/api/leases/{claim_id}/fail",
                     {"error_code": "acceptance_branch_failure", "phase": "agent"})
        print("  ", json.dumps(failed, ensure_ascii=False)[:160])
        evidence["lease_failure"] = failed

        print("── 4. assert the run is ended, not stalled ──")
        final = wait_for(
            lambda: (client.get_run(run_id).get("status") in ("completed", "failed")
                     and client.get_run(run_id)["status"]),
            timeout=300, interval=0.5, what="a terminal run status")
        print("  run status:", final)
        got = states(client, run_id)
        print("  nodes:", json.dumps(got, ensure_ascii=False))
        nodes = client.run_nodes(run_id)
        events = [item for item in client.run_events(run_id, limit=200)
                  if item["event_type"] == "run.failed"]
        evidence.update({"final_status": final, "node_states": got,
                         "run_failed_events": events, "leases_after": leases(run_id)})

        print("── 5. restart the supervisor and check nothing moved ──")
        systemctl("restart", "anchor-supervisor.service")
        time.sleep(6)
        after = states(client, run_id)
        print("  supervisor:", systemctl("is-active", "anchor-supervisor.service"))
        evidence["states_after_supervisor_restart"] = after

        failures = []
        if final != "failed":
            failures.append(f"the run ended {final}, not failed")
        non_terminal = {node: state for node, state in got.items() if state not in TERMINAL}
        if non_terminal:
            failures.append(f"nodes left without a terminal state: {non_terminal}")
        reasons = {item["node_id"]: item.get("error_code") for item in nodes}
        for node, state in got.items():
            if state == "cancelled" and reasons.get(node) != "run_failed":
                failures.append(f"{node} was abandoned without recording why: {reasons.get(node)}")
        if reasons.get(victim_node) != "acceptance_branch_failure":
            failures.append(f"the failing branch kept its own reason: {reasons.get(victim_node)}")
        if got.get("start") != "completed":
            failures.append(f"a node that really completed was disturbed: {got.get('start')}")
        if evidence["leases_after"]:
            failures.append(f"{len(evidence['leases_after'])} leases survived the failure")
        if not events:
            failures.append("no run.failed event was recorded")
        elif "abandoned_nodes" not in events[0]["payload"]:
            failures.append("the failure does not record how many nodes it ended")
        if after != got:
            failures.append(f"a supervisor restart changed the run: {after} != {got}")

        print()
        report = {**evidence, "node_reasons": reasons,
                  "gaps_in_the_agent_surface": [
                      "anchor.client exposes no lease observation or failure method, so this "
                      "acceptance reaches /api/leases directly; an agent cannot do it at all"],
                  "failures": failures}
        print(json.dumps(report, ensure_ascii=False, indent=2))
        out = ROOT / ".local" / "reports" / f"fanout-{run_id}.json"
        out.parent.mkdir(parents=True, exist_ok=True)
        out.write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8")
        print("written:", out)
        if failures:
            print("\nFAILED:", "; ".join(failures))
            return 1
        print("\nPASSED")
        return 0


if __name__ == "__main__":
    raise SystemExit(main())
