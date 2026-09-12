#!/usr/bin/env python
"""Acceptance for P0.1: a live lease survives losing its worker, and recovery closes the run.

The claim is not "the service restarts". It is that a run which loses the process holding its
lease reaches a defined, auditable state rather than sitting in `running` forever — which is
what a transient unit produced, silently, until somebody noticed no work was happening.

Against the running services:

  1. publish a one-agent graph and admit a run
  2. wait for a worker to claim it, so a lease is live
  3. stop the worker mid-node, leaving the lease without a heartbeat
  4. wait past the stale threshold and assert the lease is *reported stale* rather than
     silently reclaimed — a lease that disappears is worse than one that is late
  5. recover it explicitly and restart the worker, then wait for the run to close
  6. assert a terminal status, a continuous event sequence, and no duplicated operation

Uses `anchor.client` for everything the agent surface exposes. Lease observation and recovery
have no client method yet — that gap is recorded in the output rather than papered over.

    ANCHOR_DATABASE_URL=... .venv/bin/python scripts/validate_service_recovery.py
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
STALE_AFTER = float(os.environ.get("ANCHOR_LEASE_STALE_AFTER", "30"))
AGENT = os.environ.get("ANCHOR_ACCEPTANCE_AGENT", "agents.academic.planner")
TOKEN = (ROOT / ".local" / "api-token").read_text().strip()
TERMINAL = ("completed", "failed", "cancelled")


def lease_api(method: str, path: str, body: Any = None) -> Any:
    """Lease endpoints, which the client does not expose (see the report's `gaps`)."""
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


def wait_for(predicate, *, timeout: float, interval: float = 1.0, what: str) -> Any:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(interval)
    raise SystemExit(f"timed out after {timeout:.0f}s waiting for {what}")


def assessments(run_id: str, *, stale_after: float) -> list[dict]:
    """Active leases for this run.

    Each entry nests the lease under ``lease`` and carries an assessment beside it:
    ``state`` (healthy/stale/...) and ``recoverable``. Reading them as flat lease fields
    finds nothing and looks like "no lease was ever held".
    """
    entries = lease_api("GET", f"/api/leases/active?stale_after={stale_after:.0f}"
                               f"&run_id={run_id}") or []
    return [entry for entry in entries
            if str((entry.get("lease") or {}).get("run_id")) == run_id]


def main() -> int:
    evidence: dict[str, Any] = {"api": API, "stale_after": STALE_AFTER, "agent": AGENT}
    stamp = int(time.time())
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

        print("── 1. publish a one-agent graph ──")
        installed = client.install({
            "graph_id": f"recovery-{stamp}",
            "name": "Recovery acceptance",
            "entry_node_id": "work",
            "nodes": [{"id": "work", "type": "agent", "name": "Work", "agent_ref": AGENT}],
            "edges": [],
        })
        version_id = installed["version"]["graph_version_id"]
        print("  version:", version_id)
        evidence["graph_version_id"] = version_id
        trigger = client.register_trigger(version_id)  # the server assigns the id
        print("  trigger:", trigger["id"])

        print("── 2. admit a run and wait for a live lease ──")
        run = client.start_run(trigger["id"],
                               objective=("Plan a literature review on cost models in "
                                          "compiler optimization."),
                               idempotency_key=f"recovery-{stamp}")
        run_id = run["run_id"]
        print("  run:", run_id)
        evidence["run_id"] = run_id

        # A node that is one model call long holds its lease for about three seconds, so
        # the poll has to be tight enough to catch it — the window is the whole test.
        lease = wait_for(
            lambda: (assessments(run_id, stale_after=600) or [None])[0],
            timeout=180, interval=0.05, what="a worker to claim the node")
        claim_id = lease["lease"]["claim_id"]
        print(f"   claim {claim_id} on node {lease['lease'].get('node_id')}"
              f" ({lease.get('state')})")
        evidence["lease_at_claim"] = lease

        print("── 3. stop the worker mid-node ──")
        systemctl("stop", "anchor-worker.service")
        print("   worker state:", systemctl("is-active", "anchor-worker.service"))

        print(f"── 4. wait past the stale threshold ({STALE_AFTER:.0f}s) ──")
        time.sleep(STALE_AFTER + 5)
        active = assessments(run_id, stale_after=STALE_AFTER)
        print("   assessments:", json.dumps(active, ensure_ascii=False)[:220])
        evidence["after_threshold"] = active
        if not active:
            print("   the lease vanished without recovery: a silent reclaim, which is exactly")
            print("   what must not happen")
            return 1
        if active[0].get("state") != "stale":
            print(f"   not reported stale after {STALE_AFTER:.0f}s: {active[0]}")
            return 1
        print("   reported:", active[0].get("state"), "| recoverable:",
              active[0].get("recoverable"))

        print("── 5. recover explicitly and restart the worker ──")
        recovered = lease_api("POST", f"/api/leases/{claim_id}/recover",
                              {"reason": "acceptance: worker stopped mid-node"})
        print("  ", json.dumps(recovered, ensure_ascii=False)[:200])
        evidence["recovery"] = recovered
        systemctl("start", "anchor-worker.service")

        print("── 6. wait for the run to close ──")
        final = wait_for(
            lambda: (client.get_run(run_id).get("status") in TERMINAL
                     and client.get_run(run_id).get("status")),
            timeout=420, interval=2.0, what="a terminal run status")
        print("   final status:", final)
        evidence["final_status"] = final

        print("── evidence ──")
        nodes = client.run_nodes(run_id)
        # The API caps a page at 200, and asking for more is a 422 rather than a clamp.
        events: list[dict] = []
        while True:
            page = client.run_events(run_id, after=len(events), limit=200)
            events.extend(page)
            if len(page) < 200:
                break
        operations = client.run_operations(run_id)
        sequences = [item["sequence"] for item in events if "sequence" in item]
        gaps = [b - a for a, b in zip(sequences, sequences[1:]) if b - a != 1]
        operation_ids = [item.get("operation_id") for item in operations]
        report = {
            **evidence,
            "nodes": [{"node": n.get("node_id"), "attempt": n.get("attempt"),
                       "status": n.get("status"), "error_code": n.get("error_code")}
                      for n in nodes],
            "event_count": len(sequences),
            "event_sequence_gaps": gaps,
            "operation_count": len(operation_ids),
            "duplicate_operations": len(operation_ids) - len(set(operation_ids)),
            "gaps_in_the_agent_surface": [
                "anchor.client exposes no lease observation or recovery method, so this "
                "acceptance reaches /api/leases directly; an agent cannot do it at all",
            ],
        }
        print(json.dumps(report, ensure_ascii=False, indent=2))
        out = ROOT / ".local" / "reports" / f"recovery-{run_id}.json"
        out.parent.mkdir(parents=True, exist_ok=True)
        out.write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8")
        print("written:", out)

        failures = []
        if final not in TERMINAL:
            failures.append(f"run ended as {final}")
        if gaps:
            failures.append(f"event sequence has gaps: {gaps}")
        if report["duplicate_operations"]:
            failures.append(f"{report['duplicate_operations']} duplicated operations")
        if not nodes:
            failures.append("no node runs recorded")
        if failures:
            print("\nFAILED:", "; ".join(failures))
            return 1
        print("\nPASSED")
        return 0


if __name__ == "__main__":
    raise SystemExit(main())
