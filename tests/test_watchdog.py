from datetime import datetime, timedelta, timezone

import pytest

from anchor.runtime import AdaptiveWatchdog, ProgressEvidence, RunHealth


def test_watchdog_accepts_progress_without_max_counters():
    now = datetime.now(timezone.utc)
    previous = ProgressEvidence(state_revision=1, phase="work", heartbeat_at=now)
    current = ProgressEvidence(state_revision=2, phase="work", heartbeat_at=now,
                               cycle_iteration=1000000, verified_progress_refs=("verified:change-1",))

    decision = AdaptiveWatchdog().assess(current=current, previous=previous, now=now)

    assert decision.health is RunHealth.HEALTHY
    assert decision.action == "continue"


def test_watchdog_observes_quiet_runs_and_probes_missing_heartbeats():
    now = datetime.now(timezone.utc)
    previous = ProgressEvidence(state_revision=3, phase="work", heartbeat_at=now)
    current = ProgressEvidence(state_revision=3, phase="work", heartbeat_at=now)
    watchdog = AdaptiveWatchdog(heartbeat_timeout=timedelta(seconds=10))

    stalled = watchdog.assess(current=current, previous=previous, now=now)
    disconnected = watchdog.assess(current=current, previous=previous, now=now, dependency_connected=False)
    expired = watchdog.assess(current=current, previous=previous, now=now + timedelta(seconds=11))

    assert stalled.health is RunHealth.OBSERVING
    assert stalled.action == "continue"
    assert disconnected.health is RunHealth.DISCONNECTED
    assert disconnected.action == "reconcile_then_reconnect"
    assert expired.health is RunHealth.UNKNOWN
    assert expired.action == "probe_worker_and_lease"


NOW = datetime(2026, 9, 6, tzinfo=timezone.utc)


def evidence(**changes):
    fields = {"state_revision": 1, "phase": "work", "heartbeat_at": NOW}
    fields.update(changes)
    return ProgressEvidence(**fields)


def assess(current, previous=None):
    return AdaptiveWatchdog().assess(current=current, previous=previous, now=NOW)


def test_unchanged_observations_do_not_interrupt_long_model_call():
    previous = evidence()
    for minute in range(100):
        current = evidence(heartbeat_at=NOW + timedelta(minutes=minute))
        result = AdaptiveWatchdog().assess(current=current, previous=previous, now=current.heartbeat_at)
        assert result.health is RunHealth.OBSERVING
        assert result.action == "continue"
        previous = current


def test_suspended_approval_does_not_require_a_live_worker():
    current = evidence(heartbeat_at=NOW - timedelta(days=60), waiting_for="approval", worker_expected=False)
    result = assess(current)
    assert result.health is RunHealth.BLOCKED
    assert result.action == "wait_for_signal"


def test_metadata_churn_is_not_progress():
    result = assess(evidence(state_revision=20, phase="replan", hypothesis_hash="new-words",
                             tool_operation_ids=("new-uuid",), artifact_refs=("new-unverified-file",)), evidence())
    assert result.health is RunHealth.OBSERVING


def test_repeated_completed_cycle_requests_diagnosis_not_shutdown():
    previous = evidence(cycle_iteration=1, cycle_fingerprint="same-state-request-result")
    current = evidence(cycle_iteration=2, cycle_fingerprint="same-state-request-result")
    result = assess(current, previous)
    assert result.health is RunHealth.SUSPECTED_STALL
    assert result.action == "request_diagnostic"


def test_legitimate_polling_is_a_wait_not_a_cycle_failure():
    previous = evidence(cycle_iteration=1, cycle_fingerprint="not-ready")
    current = evidence(cycle_iteration=2, cycle_fingerprint="not-ready", waiting_for="external-job")
    assert assess(current, previous).health is RunHealth.BLOCKED


def test_heartbeat_must_come_from_an_explicit_timezone_aware_observation():
    with pytest.raises(ValueError):
        ProgressEvidence(state_revision=1, phase="work")
    with pytest.raises(ValueError):
        evidence(heartbeat_at=datetime(2026, 9, 6))
