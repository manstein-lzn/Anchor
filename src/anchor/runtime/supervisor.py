"""Conservative lease supervision primitives.

The supervisor only observes leases and emits recommendations.  It never
steals an active lease or retries an external side effect automatically.
"""
from __future__ import annotations

from dataclasses import dataclass
from datetime import datetime, timedelta, timezone
from typing import Iterable

from anchor.domain.graph import RECOVERABLE_CONTROL_TYPES
from anchor.domain.models import NodeLease


@dataclass(frozen=True)
class LeaseAssessment:
    lease: NodeLease
    state: str  # healthy, stale, unknown
    recoverable: bool
    reason: str


def assess_leases(leases: Iterable[NodeLease], *, stale_after: float = 30.0,
                  now: datetime | None = None,
                  node_types: dict[str, str] | None = None) -> list[LeaseAssessment]:
    """Assess liveness without changing durable state.

    ``node_types`` maps claim ids (with a node-id compatibility fallback) to
    graph node types. Agent, Verifier, and deterministic control leases require
    operator-confirmed recovery; Tool leases remain
    unknown until reconciliation.
    """
    if stale_after <= 0:
        raise ValueError("stale_after must be positive")
    current = now or datetime.now(timezone.utc)
    types = node_types or {}
    result: list[LeaseAssessment] = []
    for lease in leases:
        heartbeat = lease.heartbeat_at
        if heartbeat.tzinfo is None:
            heartbeat = heartbeat.replace(tzinfo=timezone.utc)
        stale = current - heartbeat > timedelta(seconds=stale_after)
        kind = types.get(str(lease.claim_id), types.get(lease.node_id, "unknown"))
        if not stale:
            result.append(LeaseAssessment(lease, "healthy", False, "heartbeat is recent"))
        elif kind == "agent":
            result.append(LeaseAssessment(lease, "stale", True, "agent heartbeat is stale; operator confirmation required"))
        elif kind in {item.value for item in RECOVERABLE_CONTROL_TYPES}:
            result.append(LeaseAssessment(
                lease, "stale", True,
                "control heartbeat is stale; operator confirmation required",
            ))
        elif kind == "verifier":
            result.append(LeaseAssessment(
                lease, "stale", True,
                "verifier heartbeat is stale; operator confirmation required",
            ))
        elif kind == "tool":
            result.append(LeaseAssessment(lease, "unknown", False, "tool side effect outcome requires reconciliation"))
        else:
            result.append(LeaseAssessment(lease, "unknown", False, "node type is unavailable; no automatic recovery"))
    return result
