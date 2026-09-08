"""Adaptive watchdog tests: evidence persistence and diagnostic requests."""

from __future__ import annotations

from datetime import datetime, timedelta, timezone
from uuid import uuid4

from anchor.domain.graph import GraphDefinition, GraphVersion
from anchor.domain.models import ProgressEvidence, Run, Task
from anchor.runtime.watchdog import AdaptiveWatchdog, RunHealth, cycle_fingerprint
from conftest import make_store


def _run(store):
    task = store.create_task(Task(objective="watchdog-test"))
    definition = GraphDefinition.model_validate({
        "graph_id": "watchdog-test",
        "name": "watchdog",
        "nodes": [{"id": "start", "type": "agent", "name": "start",
                   "agent_ref": "agents.researcher"}],
        "edges": [],
    })
    version = store.publish_graph(GraphVersion.publish(definition, 1))
    return store.create_run(Run(task_id=task.id, graph_version_id=version.graph_version_id))


def _evidence(run, **overrides):
    fields = dict(
        run_id=run.id,
        state_revision=0,
        phase="node.execute",
        heartbeat_at=datetime.now(timezone.utc),
        worker_expected=True,
    )
    fields.update(overrides)
    return ProgressEvidence(**fields)


def test_initial_observation_records_evidence(tmp_path):
    store = make_store(tmp_path)
    try:
        run = _run(store)
        decision = AdaptiveWatchdog(store).assess(run_id=run.id, current=_evidence(run))
        assert decision.health == RunHealth.OBSERVING
        evidence = store.list_progress_evidence(run.id)
        assert len(evidence) == 1 and evidence[0].state_revision == 0
    finally:
        store.close()


def test_verified_progress_change_returns_healthy(tmp_path):
    store = make_store(tmp_path)
    try:
        run = _run(store)
        watchdog = AdaptiveWatchdog(store)
        watchdog.assess(run_id=run.id, current=_evidence(run, verified_progress_refs=()))
        decision = watchdog.assess(
            run_id=run.id,
            current=_evidence(run, state_revision=1,
                              verified_progress_refs=("artifact://sha256/abc",)),
        )
        assert decision.health == RunHealth.HEALTHY
    finally:
        store.close()


def test_repeated_cycle_without_progress_requests_diagnostic(tmp_path):
    store = make_store(tmp_path)
    try:
        run = _run(store)
        watchdog = AdaptiveWatchdog(store)
        fingerprint = "a" * 64
        watchdog.assess(run_id=run.id, current=_evidence(
            run, cycle_iteration=1, cycle_fingerprint=fingerprint))
        decision = watchdog.assess(run_id=run.id, current=_evidence(
            run, state_revision=1, cycle_iteration=2, cycle_fingerprint=fingerprint))
        assert decision.health == RunHealth.SUSPECTED_STALL
        diagnostics = store.list_open_diagnostics(run.id)
        assert len(diagnostics) == 1
        assert diagnostics[0].reason == decision.reason
    finally:
        store.close()


def test_waiting_state_is_blocked_without_diagnostic(tmp_path):
    store = make_store(tmp_path)
    try:
        run = _run(store)
        decision = AdaptiveWatchdog(store).assess(
            run_id=run.id, current=_evidence(run, phase="waiting_approval", waiting_for="approval"))
        assert decision.health == RunHealth.BLOCKED
        assert store.list_open_diagnostics(run.id) == []
    finally:
        store.close()


def test_overdue_heartbeat_is_unknown(tmp_path):
    store = make_store(tmp_path)
    try:
        run = _run(store)
        decision = AdaptiveWatchdog(store).assess(
            run_id=run.id,
            current=_evidence(run, heartbeat_at=datetime.now(timezone.utc) - timedelta(minutes=10)),
        )
        assert decision.health == RunHealth.UNKNOWN
    finally:
        store.close()


def test_cycle_fingerprint_ignores_noise_and_detects_change():
    base = dict(phase="research", input_hash="abc", output_refs=("r1",),
                tool_operation_ids=("t1",))
    assert cycle_fingerprint(**base) == cycle_fingerprint(**base)
    assert cycle_fingerprint(**base) != cycle_fingerprint(**{**base, "phase": "review"})
    assert cycle_fingerprint(**base) != cycle_fingerprint(**{**base, "output_refs": ("r2",)})


def test_watchdog_restart_does_not_reissue_the_same_diagnostic(tmp_path):
    """Matrix 9: supervisor/watchdog restart resumes observation without duplicates."""
    from anchor.state.relational import RelationalStateStore
    store = make_store(tmp_path)
    url = str(store.engine.url)
    run = _run(store)
    fingerprint = "b" * 64
    try:
        watchdog = AdaptiveWatchdog(store)
        watchdog.assess(run_id=run.id, current=_evidence(
            run, cycle_iteration=1, cycle_fingerprint=fingerprint))
        watchdog.assess(run_id=run.id, current=_evidence(
            run, state_revision=1, cycle_iteration=2, cycle_fingerprint=fingerprint))
        assert len(store.list_open_diagnostics(run.id)) == 1
    finally:
        store.close()
    reopened = RelationalStateStore(url)
    try:
        # A restarted observer sees the persisted evidence and open diagnostic.
        assert len(reopened.list_open_diagnostics(run.id)) == 1
        assert len(reopened.list_progress_evidence(run.id)) == 2
        AdaptiveWatchdog(reopened).assess(run_id=run.id, current=_evidence(
            run, state_revision=2, cycle_iteration=3, cycle_fingerprint=fingerprint))
        assert len(reopened.list_open_diagnostics(run.id)) == 1  # deduplicated
    finally:
        reopened.close()
