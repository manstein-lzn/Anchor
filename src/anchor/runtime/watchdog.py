"""Adaptive progress observer.

The watchdog separates worker liveness from task progress. It persists
immutable observations and raises a deduplicated diagnostic request when a
completed cycle repeats without new verified progress. It never stops a run
or transfers a lease: absence of evidence is uncertainty, not failure.
"""

from __future__ import annotations

import hashlib
from datetime import datetime, timedelta, timezone
from enum import StrEnum
from typing import Sequence
from uuid import UUID

from pydantic import BaseModel, ConfigDict

from anchor.domain.models import DiagnosticRequest, ProgressEvidence
from anchor.state.protocols import StateStore


class RunHealth(StrEnum):
    HEALTHY = "healthy"
    BLOCKED = "blocked"
    STALLED = "stalled"
    DISCONNECTED = "disconnected"
    OBSERVING = "observing"
    SUSPECTED_STALL = "suspected_stall"
    UNKNOWN = "unknown"


class WatchdogDecision(BaseModel):
    model_config = ConfigDict(extra="forbid")

    health: RunHealth
    action: str
    reason: str


def cycle_fingerprint(*, phase: str, input_hash: str | None, output_refs: tuple[str, ...],
                      tool_operation_ids: tuple[str, ...]) -> str:
    """Deterministic fingerprint of one completed cycle.

    Only durable inputs, results and tool operations participate; timestamps and
    random ids are excluded so activity cannot masquerade as progress.
    """
    payload = "|".join([
        phase,
        input_hash or "",
        ",".join(sorted(output_refs)),
        ",".join(sorted(tool_operation_ids)),
    ])
    return hashlib.sha256(payload.encode("utf-8")).hexdigest()


class AdaptiveWatchdog:
    """Persistent progress observer with durable evidence and diagnostics."""

    def __init__(self, store: StateStore, *, heartbeat_timeout: timedelta = timedelta(minutes=5)) -> None:
        if heartbeat_timeout <= timedelta(0):
            raise ValueError("heartbeat_timeout must be positive")
        self.store = store
        self.heartbeat_timeout = heartbeat_timeout

    def _latest_evidence(self, run_id: UUID) -> ProgressEvidence | None:
        items = self.store.list_progress_evidence(run_id)
        if not items:
            return None
        return max(items, key=lambda item: (item.state_revision, item.heartbeat_at))

    def _open_diagnostics(self, run_id: UUID) -> Sequence[DiagnosticRequest]:
        return self.store.list_open_diagnostics(run_id)

    def assess(
        self,
        *,
        run_id: UUID,
        current: ProgressEvidence,
        now: datetime | None = None,
        dependency_connected: bool = True,
    ) -> WatchdogDecision:
        observed_at = now or datetime.now(timezone.utc)
        if observed_at.tzinfo is None or observed_at.utcoffset() is None:
            raise ValueError("now must be timezone-aware")
        previous = self._latest_evidence(run_id)
        if (previous is not None and previous.heartbeat_at == current.heartbeat_at
                and previous.state_revision == current.state_revision):
            return WatchdogDecision(
                health=RunHealth.OBSERVING, action="continue",
                reason="evidence already recorded for this state revision",
            )
        decision = self._decide(current=current, previous=previous, now=observed_at,
                                dependency_connected=dependency_connected)
        self.store.append_progress_evidence(current)
        if decision.action == "request_diagnostic":
            self._request_diagnostic(run_id, current, decision, observed_at)
        return decision

    def _request_diagnostic(self, run_id: UUID, current: ProgressEvidence,
                            decision: WatchdogDecision, observed_at: datetime) -> None:
        # Deduplicate by reason, not by state revision: a restarted observer
        # must not re-issue an equivalent diagnosis while one is still open.
        reason_digest = hashlib.sha256(decision.reason.encode()).hexdigest()[:12]
        diagnostic_id = f"diagnostic:{run_id}:{reason_digest}"
        if any(item.diagnostic_id == diagnostic_id for item in self._open_diagnostics(run_id)):
            return
        self.store.add_diagnostic_request(DiagnosticRequest(
            diagnostic_id=diagnostic_id,
            run_id=run_id,
            node_run_id=current.node_run_id,
            reason=decision.reason,
            evidence_refs=current.verified_progress_refs[:10],
            suggested_actions=("operator_review",),
            created_at=observed_at,
        ))

    def _decide(
        self,
        *,
        current: ProgressEvidence,
        previous: ProgressEvidence | None,
        now: datetime,
        dependency_connected: bool,
    ) -> WatchdogDecision:
        if not dependency_connected:
            return WatchdogDecision(
                health=RunHealth.DISCONNECTED, action="reconcile_then_reconnect",
                reason="dependency disconnected; side-effect outcome may be unknown",
            )
        if current.worker_expected and now - current.heartbeat_at > self.heartbeat_timeout:
            return WatchdogDecision(
                health=RunHealth.UNKNOWN, action="probe_worker_and_lease",
                reason="heartbeat overdue; this does not prove the worker stopped",
            )
        if current.waiting_for:
            return WatchdogDecision(
                health=RunHealth.BLOCKED, action="wait_for_signal",
                reason=f"declared wait: {current.waiting_for}",
            )
        if not current.worker_expected:
            return WatchdogDecision(
                health=RunHealth.UNKNOWN, action="inspect_wait_state",
                reason="no active worker and no declared wait",
            )
        if previous is None:
            return WatchdogDecision(
                health=RunHealth.OBSERVING, action="continue",
                reason="initial observation; liveness alone is not task progress",
            )
        if set(current.verified_progress_refs) - set(previous.verified_progress_refs):
            return WatchdogDecision(
                health=RunHealth.HEALTHY, action="continue",
                reason="new independently verified progress evidence",
            )
        if (current.cycle_fingerprint is not None
                and current.cycle_fingerprint == previous.cycle_fingerprint
                and current.cycle_iteration > previous.cycle_iteration):
            return WatchdogDecision(
                health=RunHealth.SUSPECTED_STALL, action="request_diagnostic",
                reason="completed cycle repeated without verified progress; do not interrupt automatically",
            )
        return WatchdogDecision(
            health=RunHealth.OBSERVING, action="continue",
            reason="insufficient evidence of progress or failure; keep observing",
        )
