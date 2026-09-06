from datetime import datetime, timedelta, timezone
from uuid import uuid4

import pytest

from anchor.domain.models import NodeLease
from anchor.runtime.supervisor import assess_leases
from anchor.runtime.supervisor_service import run_supervisor
import asyncio


def lease(node_id, heartbeat):
    now = heartbeat
    return NodeLease(claim_id=uuid4(), node_run_id=uuid4(), run_id=uuid4(),
                     node_id=node_id, worker_id="w", acquired_at=now,
                     heartbeat_at=heartbeat)


def test_stale_agent_is_recoverable_but_not_mutated():
    now = datetime.now(timezone.utc)
    assessment = assess_leases([lease("a", now - timedelta(seconds=60))],
                               now=now, node_types={"a": "agent"})[0]
    assert (assessment.state, assessment.recoverable) == ("stale", True)


def test_stale_tool_is_unknown_and_not_recoverable():
    now = datetime.now(timezone.utc)
    assessment = assess_leases([lease("t", now - timedelta(seconds=60))],
                               now=now, node_types={"t": "tool"})[0]
    assert (assessment.state, assessment.recoverable) == ("unknown", False)
    assert "reconciliation" in assessment.reason


def test_stale_control_node_requires_operator_confirmed_recovery():
    now = datetime.now(timezone.utc)
    assessment = assess_leases([lease("route", now - timedelta(seconds=60))],
                               now=now, node_types={"route": "router"})[0]
    assert (assessment.state, assessment.recoverable) == ("stale", True)
    assert "operator confirmation" in assessment.reason


def test_stale_verifier_requires_operator_confirmed_recovery():
    now = datetime.now(timezone.utc)
    assessment = assess_leases([lease("verify", now - timedelta(seconds=60))],
                               now=now, node_types={"verify": "verifier"})[0]
    assert (assessment.state, assessment.recoverable) == ("stale", True)
    assert "operator confirmation" in assessment.reason


def test_claim_identity_prevents_same_node_id_type_collision():
    now = datetime.now(timezone.utc)
    agent = lease("shared", now - timedelta(seconds=60))
    tool = lease("shared", now - timedelta(seconds=60))
    assessments = assess_leases(
        [agent, tool],
        now=now,
        node_types={str(agent.claim_id): "agent", str(tool.claim_id): "tool"},
    )
    assert assessments[0].recoverable is True
    assert assessments[1].state == "unknown" and assessments[1].recoverable is False


def test_invalid_threshold_rejected():
    with pytest.raises(ValueError):
        assess_leases([], stale_after=0)


def test_supervisor_service_observes_until_stopped():
    now = datetime.now(timezone.utc)
    class Store:
        def __init__(self): self.calls = 0
        def list_active_leases(self): self.calls += 1; return [lease("a", now - timedelta(seconds=60))]
        def get_run(self, _): return None
        def get_graph_version(self, _): return None
    async def scenario():
        store = Store(); stop = asyncio.Event()
        async def stop_soon():
            await asyncio.sleep(0.01); stop.set()
        await asyncio.gather(run_supervisor(store, interval=0.001, stale_after=1, stop=stop), stop_soon())
        return store.calls
    assert asyncio.run(scenario()) > 0
