from __future__ import annotations

from datetime import datetime, timedelta, timezone
from enum import StrEnum
from hashlib import sha256
from pydantic import AwareDatetime, BaseModel, ConfigDict, Field


class RunHealth(StrEnum):
    HEALTHY = "healthy"
    BLOCKED = "blocked"
    STALLED = "stalled"
    DISCONNECTED = "disconnected"
    OBSERVING = "observing"
    SUSPECTED_STALL = "suspected_stall"
    UNKNOWN = "unknown"


class ProgressEvidence(BaseModel):
    model_config = ConfigDict(extra="forbid")

    state_revision: int = Field(ge=0)
    phase: str
    artifact_refs: tuple[str, ...] = ()
    verifier_passes: int = Field(default=0, ge=0)
    hypothesis_hash: str | None = None
    tool_operation_ids: tuple[str, ...] = ()
    heartbeat_at: AwareDatetime
    waiting_for: str | None = None
    worker_expected: bool = True
    verified_progress_refs: tuple[str, ...] = ()
    cycle_iteration: int = Field(default=0, ge=0)
    cycle_fingerprint: str | None = None

    @classmethod
    def from_hypothesis(cls, *, state_revision: int, phase: str, hypothesis: str, **kwargs: object) -> ProgressEvidence:
        return cls(
            state_revision=state_revision,
            phase=phase,
            hypothesis_hash=sha256(hypothesis.encode("utf-8")).hexdigest(),
            **kwargs,
        )


class WatchdogDecision(BaseModel):
    model_config = ConfigDict(extra="forbid")

    health: RunHealth
    action: str
    reason: str


class AdaptiveWatchdog:
    """Advisory rule-based prototype; does not stop runs or transfer leases.

    Heartbeats describe liveness, not task quality. Cycle fingerprints must come
    from completed-cycle inputs, results and relevant state, not operation UUIDs
    or the agent's assertion that it is making progress.
    """

    def __init__(self, heartbeat_timeout: timedelta = timedelta(minutes=5)) -> None:
        if heartbeat_timeout <= timedelta(0):
            raise ValueError("heartbeat_timeout must be positive")
        self.heartbeat_timeout = heartbeat_timeout

    def assess(
        self,
        *,
        current: ProgressEvidence,
        previous: ProgressEvidence | None,
        now: datetime | None = None,
        dependency_connected: bool = True,
    ) -> WatchdogDecision:
        observed_at = now or datetime.now(timezone.utc)
        if observed_at.tzinfo is None or observed_at.utcoffset() is None:
            raise ValueError("now must be timezone-aware")
        if not dependency_connected:
            return WatchdogDecision(health=RunHealth.DISCONNECTED, action="reconcile_then_reconnect", reason="dependency disconnected; side-effect outcome may be unknown")
        if current.worker_expected and observed_at - current.heartbeat_at > self.heartbeat_timeout:
            return WatchdogDecision(health=RunHealth.UNKNOWN, action="probe_worker_and_lease", reason="heartbeat overdue; this does not prove the worker stopped")
        if current.waiting_for:
            return WatchdogDecision(health=RunHealth.BLOCKED, action="wait_for_signal", reason=f"declared wait: {current.waiting_for}")
        if not current.worker_expected:
            return WatchdogDecision(health=RunHealth.UNKNOWN, action="inspect_wait_state", reason="no active worker and no declared wait")
        if previous is None:
            return WatchdogDecision(health=RunHealth.OBSERVING, action="continue", reason="initial observation; liveness alone is not task progress")

        progress_changed = bool(set(current.verified_progress_refs) - set(previous.verified_progress_refs))
        if progress_changed:
            return WatchdogDecision(health=RunHealth.HEALTHY, action="continue", reason="new independently verified progress evidence")
        if (current.cycle_fingerprint is not None
                and current.cycle_fingerprint == previous.cycle_fingerprint
                and current.cycle_iteration > previous.cycle_iteration):
            return WatchdogDecision(health=RunHealth.SUSPECTED_STALL, action="request_diagnostic", reason="completed cycle repeated without verified progress; do not interrupt automatically")
        return WatchdogDecision(health=RunHealth.OBSERVING, action="continue", reason="insufficient evidence of progress or failure; keep observing")
