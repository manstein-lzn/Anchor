import asyncio
import json
from datetime import timedelta
from uuid import uuid4

import pytest

from anchor.domain.models import utc_now
from anchor.runtime.academic import register_academic_behaviors
from anchor.runtime.behaviors import BehaviorRegistry
from anchor.runtime.capabilities import AgentCapability, CapabilityRegistry, ModelProfile
from anchor.runtime.model_gateway import ModelResponse
from anchor.runtime.worker import AgentNodeWorker, FailureClass, classify_failure, is_retryable_model_error
from anchor.runtime.sinks import ArtifactCheckpointSink
from anchor.state.relational import RelationalStateStore
from test_academic_research import setup, complete, claim, ledger, manuscript


class Gateway:
    def __init__(self, outputs):
        self.outputs = iter(outputs)
        self.calls = 0

    async def generate(self, **kwargs):
        self.calls += 1
        return ModelResponse(text=next(self.outputs), provider="test", model="test")


def agent_worker(store, artifacts, gateway, role="planner", timeout=600, max_retries=0):
    capability = AgentCapability(ref="academic", model_ref="test", output_format="json",
                                 behavior_ref=f"academic.{role}", timeout_seconds=timeout,
                                 max_retries=max_retries)
    registry = CapabilityRegistry(agents=[capability], models=[ModelProfile(
        ref="test", provider="test", model="test", secret_ref="unused")])
    behaviors = BehaviorRegistry()
    register_academic_behaviors(behaviors)
    return AgentNodeWorker(store, registry, {"test": gateway},
                           ArtifactCheckpointSink(store, artifacts, "agent"), behaviors=behaviors)


def execute(worker, lease, snapshot=None):
    return asyncio.run(worker.execute_claimed_once(worker_id="agent", lease=lease,
        agent_ref="academic", prompt="task", input_snapshot=snapshot or {}, heartbeat_interval=0.01))


def test_invalid_json_repairs_once_without_replaying_tools(tmp_path):
    store, artifacts, receipt, control = setup(tmp_path)
    try:
        asyncio.run(control.execute_once(worker_id="control"))
        gateway = Gateway(['{"round":', '{"round": 1}'])
        capability = AgentCapability(ref="academic", model_ref="test", output_format="json",
                                     behavior_ref="academic.planner", timeout_seconds=600,
                                     max_retries=0, output_retries=1)
        worker = agent_worker(store, artifacts, gateway)
        worker.registry = CapabilityRegistry(agents=[capability], models=[ModelProfile(
            ref="test", provider="test", model="test", secret_ref="unused")])
        result = execute(worker, claim(store, "plan"))
        assert json.loads(result.response.text) == {"round": 1}
        assert gateway.calls == 2
        assert next(n for n in store.list_node_runs(receipt.run_id) if n.node_id == "plan").status.value == "completed"
    finally:
        store.close()


def test_invalid_json_fails_explicitly_and_retains_rejected_evidence(tmp_path):
    store, artifacts, receipt, control = setup(tmp_path)
    try:
        asyncio.run(control.execute_once(worker_id="control"))
        gateway = Gateway(['{"round":', 'not JSON'])
        lease = claim(store, "plan")
        with pytest.raises(ValueError, match="agent_output_invalid"):
            execute(agent_worker(store, artifacts, gateway), lease)
        # With default output_retries=0, invalid JSON fails immediately after
        # the first model call; repair is opt-in via explicit output_retries.
        assert gateway.calls == 1
        assert store.get_run(receipt.run_id).status.value == "failed"
        assert not store.list_active_leases()
        snapshot = store.get_context_snapshot(lease.node_run_id).snapshot
        refs = snapshot['failure']['rejected_output_refs']
        assert [artifacts.get_text(ref) for ref in refs] == ['{"round":']
    finally:
        store.close()


def test_timeout_fails_instead_of_leaving_running_lease(tmp_path):
    class Slow:
        async def generate(self, **kwargs):
            await asyncio.sleep(2)
    store, artifacts, receipt, control = setup(tmp_path)
    try:
        asyncio.run(control.execute_once(worker_id="control"))
        with pytest.raises(TimeoutError):
            execute(agent_worker(store, artifacts, Slow(), timeout=0.02), claim(store, "plan"))
        assert store.get_run(receipt.run_id).status.value == 'failed'
        assert not store.list_active_leases()
        assert next(n for n in store.list_node_runs(receipt.run_id) if n.node_id == 'plan').error_code == 'agent_timeout'
    finally:
        store.close()


def test_preflight_skips_expensive_reviewer_and_enters_revision(tmp_path):
    store, artifacts, receipt, control = setup(tmp_path)
    try:
        asyncio.run(control.execute_once(worker_id="control"))
        complete(store, artifacts, claim(store, 'plan'), {'round': 1})
        asyncio.run(control.execute_once(worker_id="control"))  # coverage seed
        complete(store, artifacts, claim(store, 'gather'), ledger())
        asyncio.run(control.execute_once(worker_id="control"))  # coverage -> write
        work = {'manuscript': manuscript(), 'thesis': 't'}
        complete(store, artifacts, claim(store, 'write'), work)
        gateway = Gateway([])
        execute(agent_worker(store, artifacts, gateway, role='reviewer'), claim(store, 'review'),
                {'manuscript': work, 'evidence': ledger(),
                 'request': {'minimum_sources': 1, 'minimum_reads': 0}})
        assert gateway.calls == 0
        asyncio.run(control.execute_once(worker_id="control"))
        assert claim(store, 'plan').node_id == 'plan', 'an evidence gap returns to planning'
    finally:
        store.close()


def test_control_validation_error_becomes_failed_state(tmp_path):
    store, artifacts, receipt, control = setup(tmp_path)
    try:
        asyncio.run(control.execute_once(worker_id="control"))
        complete(store, artifacts, claim(store, 'plan'), {'round': 1})
        asyncio.run(control.execute_once(worker_id="control"))  # coverage seed
        complete(store, artifacts, claim(store, 'gather'), ledger())
        asyncio.run(control.execute_once(worker_id="control"))  # coverage -> write
        complete(store, artifacts, claim(store, 'write'), {'manuscript': manuscript(), 'thesis': 't'})
        complete(store, artifacts, claim(store, 'review'), {'verdict': 'invalid'})
        with pytest.raises(ValueError):
            asyncio.run(control.execute_once(worker_id="control"))
        assert store.get_run(receipt.run_id).status.value == 'failed'
        assert not store.list_active_leases()
    finally:
        store.close()


def test_stop_fences_claim_and_preserves_outputs(tmp_path):
    store, artifacts, receipt, control = setup(tmp_path)
    try:
        start = asyncio.run(control.execute_once(worker_id="control"))
        lease = claim(store, 'plan')
        stopped = store.stop_run(receipt.run_id, reason='user stop')
        assert stopped.status.value == 'cancelled'
        assert store.stop_run(receipt.run_id, reason='retry') == stopped
        assert not store.list_active_leases()
        assert artifacts.get_text(start.output_ref)
        assert store.claim_ready_agent_node('agent', uuid4()) is None
        with pytest.raises(Exception):
            complete(store, artifacts, lease, {'round': 1})
    finally:
        store.close()


def test_total_budget_expires_even_without_an_active_worker(tmp_path):
    from anchor.state import schema
    import sqlalchemy as sa
    store, artifacts, receipt, control = setup(tmp_path, metadata={"run_timeout_seconds": "1800"})
    try:
        with store.engine.begin() as connection:
            connection.execute(sa.update(schema.runs).where(schema.runs.c.id == str(receipt.run_id)).values(
                created_at=utc_now() - timedelta(hours=1)))
        assert store.expire_run_budgets() == 1
        assert store.get_run(receipt.run_id).status.value == 'failed'
        assert store.get_run(receipt.run_id).current_phase == 'execution_budget_exceeded'
    finally:
        store.close()


def test_revision_budget_parks_changing_drafts_after_three_rounds(tmp_path):
    store, artifacts, receipt, control = setup(tmp_path, metadata={"max_rounds": "3"})
    try:
        asyncio.run(control.execute_once(worker_id='control'))
        complete(store, artifacts, claim(store, 'plan'), {'round': 1})
        asyncio.run(control.execute_once(worker_id="control"))  # coverage seed
        complete(store, artifacts, claim(store, 'gather'), ledger())
        asyncio.run(control.execute_once(worker_id="control"))  # coverage -> write
        for round_number in range(3):
            complete(store, artifacts, claim(store, 'write'), {
                'manuscript': manuscript() + str(round_number), 'thesis': 't'})
            complete(store, artifacts, claim(store, 'review'),
                     {'verdict': 'revise', 'target': 'manuscript'})
            asyncio.run(control.execute_once(worker_id='control'))
        assert store.list_waiting_nodes(receipt.run_id)[0].node_id == 'needs_input'
        gate = max((n for n in store.list_node_runs(receipt.run_id) if n.node_id == 'check'), key=lambda n: n.attempt)
        assert 'budget exhausted' in artifacts.get_text(gate.output_ref)
    finally:
        store.close()


def test_stop_before_dispatch_does_not_block_delivery_queue(tmp_path):
    from anchor.runtime.dispatch import dispatch_pending
    from anchor.runtime.receiver import DurableExecutionReceiver
    from anchor.domain.admission import RunRequest
    from anchor.domain.graph import Trigger
    store, artifacts, receipt, control = setup(tmp_path)
    try:
        run = store.get_run(receipt.run_id)
        trigger = store.create_trigger(Trigger(graph_version_id=run.graph_version_id, type='manual'))
        fresh = store.admit_run(RunRequest(trigger_id=trigger.id, idempotency_key='stop-before-dispatch', objective='stop'))
        store.stop_run(fresh.run_id, reason='stop immediately')
        assert asyncio.run(dispatch_pending(store, DurableExecutionReceiver(store))) == 1
        assert store.get_run(fresh.run_id).status.value == 'cancelled'
    finally:
        store.close()


def test_stopping_live_agent_cancels_call_without_publishing(tmp_path):
    store, artifacts, receipt, control = setup(tmp_path)
    try:
        asyncio.run(control.execute_once(worker_id='control'))
        lease = claim(store, 'plan')
        async def scenario():
            entered = asyncio.Event()
            cancelled = asyncio.Event()
            class Slow:
                async def generate(self, **kwargs):
                    entered.set()
                    try:
                        await asyncio.sleep(10)
                    finally:
                        cancelled.set()
            worker = agent_worker(store, artifacts, Slow())
            task = asyncio.create_task(worker.execute_claimed_once(worker_id='agent', lease=lease,
                agent_ref='academic', prompt='test', heartbeat_interval=0.01))
            await entered.wait()
            store.stop_run(receipt.run_id, reason='stop active call')
            await asyncio.wait_for(task, 1)
            assert cancelled.is_set()
        asyncio.run(scenario())
        assert store.get_run(receipt.run_id).status.value == 'cancelled'
        assert next(n for n in store.list_node_runs(receipt.run_id) if n.node_id == 'plan').output_ref is None
    finally:
        store.close()


def test_provider_error_becomes_explicit_academic_failure(tmp_path):
    class Broken:
        async def generate(self, **kwargs):
            raise RuntimeError('Empty provider response')
    store, artifacts, receipt, control = setup(tmp_path)
    try:
        asyncio.run(control.execute_once(worker_id='control'))
        with pytest.raises(RuntimeError):
            execute(agent_worker(store, artifacts, Broken()), claim(store, 'plan'))
        assert store.get_run(receipt.run_id).status.value == 'failed'
        assert not store.list_active_leases()
    finally:
        store.close()


def test_transient_provider_error_queues_a_new_attempt(tmp_path):
    class Flaky:
        def __init__(self):
            self.calls = 0

        async def generate(self, **kwargs):
            self.calls += 1
            if self.calls == 1:
                raise RuntimeError("HTTP 504 Gateway Time-out")
            return ModelResponse(text='{"round": 1}', provider="test", model="test")

    store, artifacts, receipt, control = setup(tmp_path)
    try:
        asyncio.run(control.execute_once(worker_id="control"))
        gateway = Flaky()
        worker = agent_worker(store, artifacts, gateway, max_retries=2)
        worker.retry_backoff_seconds = (0,)
        first = claim(store, "plan")
        with pytest.raises(RuntimeError, match="HTTP 504"):
            execute(worker, first)
        attempts = sorted((node for node in store.list_node_runs(receipt.run_id)
                           if node.node_id == "plan"), key=lambda node: node.attempt)
        assert [node.status.value for node in attempts] == ["failed", "ready"]
        second = claim(store, "plan")
        execute(worker, second)
        assert gateway.calls == 2
        assert max(node.attempt for node in store.list_node_runs(receipt.run_id)
                   if node.node_id == "plan") == 1
    finally:
        store.close()


def test_transient_failure_persists_recovery_schedule_across_reopen(tmp_path):
    class Flaky:
        async def generate(self, **kwargs):
            raise RuntimeError("HTTP 503 Service Unavailable")

    store, artifacts, receipt, control = setup(tmp_path)
    try:
        asyncio.run(control.execute_once(worker_id="control"))
        worker = agent_worker(store, artifacts, Flaky(), max_retries=2)
        worker.retry_backoff_seconds = (60.0,)
        with pytest.raises(RuntimeError, match="HTTP 503"):
            execute(worker, claim(store, "plan"))
        attempts = sorted((node for node in store.list_node_runs(receipt.run_id)
                           if node.node_id == "plan"), key=lambda node: node.attempt)
        failed, fresh = attempts
        # The failed attempt records the classified fault, not a business round.
        assert failed.status.value == "failed"
        assert failed.last_error_class == "transient_http"
        # The fresh attempt is scheduled, not immediately claimable.
        assert fresh.status.value == "ready"
        assert fresh.next_attempt_at is not None
        assert store.claim_ready_agent_node("agent", uuid4()) is None
        event = next(item for item in store.list_events(receipt.run_id)
                     if item["event_type"] == "node.retrying")
        assert event["payload"]["error_class"] == "transient_http"
        assert event["payload"]["next_attempt_at"] is not None
    finally:
        store.close()
    # A reopened store must still see the schedule and refuse an early claim.
    reopened = RelationalStateStore(f"sqlite:///{tmp_path / 'anchor.sqlite'}")
    try:
        fresh = max((node for node in reopened.list_node_runs(receipt.run_id)
                     if node.node_id == "plan"), key=lambda node: node.attempt)
        assert fresh.next_attempt_at is not None
        assert reopened.claim_ready_agent_node("agent", uuid4()) is None
    finally:
        reopened.close()


def test_failure_classification_separates_transient_from_configuration():
    class HttpStatus(Exception):
        def __init__(self, status_code):
            self.status_code = status_code

    assert classify_failure(HttpStatus(503)) == FailureClass.TRANSIENT_HTTP
    assert classify_failure(HttpStatus(429)) == FailureClass.TRANSIENT_HTTP
    assert classify_failure(HttpStatus(401)) == FailureClass.AUTHENTICATION
    assert classify_failure(HttpStatus(403)) == FailureClass.AUTHORIZATION
    assert classify_failure(HttpStatus(422)) == FailureClass.MODEL_REJECTED
    assert classify_failure(TimeoutError("connect timeout")) == FailureClass.TRANSIENT_NETWORK
    assert classify_failure(RuntimeError("HTTP 504 Gateway Time-out")) == FailureClass.TRANSIENT_HTTP
    assert classify_failure(RuntimeError("unknown model")) == FailureClass.CONFIGURATION


def test_only_transient_failures_are_retryable():
    assert is_retryable_model_error(RuntimeError("HTTP 503")) is True
    assert is_retryable_model_error(TimeoutError("read timeout")) is True
    assert is_retryable_model_error(RuntimeError("HTTP 401 unauthorized")) is False
    assert is_retryable_model_error(RuntimeError("HTTP 422 unprocessable")) is False


# ---------------------------------------------------------------------------
# EXECUTION_POLICY_PLAN acceptance matrix
# ---------------------------------------------------------------------------

def test_more_than_three_revision_rounds_continue_without_hidden_cap(tmp_path):
    """Matrix 1: no implicit round limit; a 4th revise cycle keeps running."""
    store, artifacts, receipt, control = setup(tmp_path)  # no max_rounds in metadata
    try:
        asyncio.run(control.execute_once(worker_id="control"))
        complete(store, artifacts, claim(store, "plan"), {"round": 1})
        asyncio.run(control.execute_once(worker_id="control"))  # coverage seed
        complete(store, artifacts, claim(store, "gather"), ledger())
        asyncio.run(control.execute_once(worker_id="control"))  # coverage -> write
        for round_number in range(4):
            complete(store, artifacts, claim(store, "write"),
                     {"manuscript": manuscript() + str(round_number), "thesis": "t"})
            complete(store, artifacts, claim(store, "review"),
                     {"verdict": "revise", "target": "manuscript", "issues": []})
            asyncio.run(control.execute_once(worker_id="control"))
        run = store.get_run(receipt.run_id)
        assert run.status.value == "running"
        assert store.list_waiting_nodes(receipt.run_id) == []
        # A 5th writing attempt is queued; nothing auto-failed or auto-blocked.
        assert claim(store, "write").node_id == "write"
    finally:
        store.close()


def test_no_budget_means_expire_run_budgets_never_touches_the_run(tmp_path):
    """Matrix 2/13: absent explicit budget => no implicit wall-clock termination."""
    import sqlalchemy as sa
    from anchor.state import schema
    store, artifacts, receipt, control = setup(tmp_path)  # no run_timeout_seconds
    try:
        with store.engine.begin() as connection:
            connection.execute(sa.update(schema.runs).where(
                schema.runs.c.id == str(receipt.run_id)).values(
                created_at=utc_now() - timedelta(days=2)))
        assert store.expire_run_budgets() == 0
        assert store.get_run(receipt.run_id).status.value != "failed"
    finally:
        store.close()


def test_text_only_change_is_not_verified_progress_and_never_fails(tmp_path):
    """Matrix 3: artifact/text change alone is not progress and not failure."""
    from datetime import datetime, timezone
    from anchor.domain.models import ProgressEvidence, Run, Task
    from anchor.domain.graph import GraphDefinition, GraphVersion
    from anchor.runtime.watchdog import AdaptiveWatchdog, RunHealth
    task = store = None
    store = __import__("conftest").make_store(tmp_path)
    try:
        task = store.create_task(Task(objective="noise"))
        definition = GraphDefinition.model_validate({
            "graph_id": "noise", "name": "noise",
            "nodes": [{"id": "start", "type": "agent", "name": "start",
                       "agent_ref": "agents.researcher"}], "edges": []})
        version = store.publish_graph(GraphVersion.publish(definition, 1))
        run = store.create_run(Run(task_id=task.id, graph_version_id=version.graph_version_id))
        watchdog = AdaptiveWatchdog(store)
        base = dict(run_id=run.id, phase="node.execute", worker_expected=True,
                    heartbeat_at=datetime.now(timezone.utc))
        watchdog.assess(run_id=run.id, current=ProgressEvidence(state_revision=0, **base))
        changed = watchdog.assess(run_id=run.id, current=ProgressEvidence(
            state_revision=1, artifact_refs=("artifact://sha256/" + "a" * 64,), **base))
        assert changed.health == RunHealth.OBSERVING
        assert store.get_run(run.id).status.value != "failed"
    finally:
        store.close()


def test_consecutive_transient_failures_recover_with_persisted_backoff(tmp_path):
    """Matrix 5: repeated 503/429 then recovery, no completed side effect repeated."""
    class Flaky:
        def __init__(self):
            self.calls = 0

        async def generate(self, **kwargs):
            self.calls += 1
            if self.calls <= 2:
                raise RuntimeError("HTTP 503 Service Unavailable")
            return ModelResponse(text='{"round": 1}', provider="test", model="test")

    store, artifacts, receipt, control = setup(tmp_path)
    try:
        asyncio.run(control.execute_once(worker_id="control"))
        gateway = Flaky()
        worker = agent_worker(store, artifacts, gateway, max_retries=3)
        worker.retry_backoff_seconds = (0,)
        for _ in range(2):
            with pytest.raises(RuntimeError, match="HTTP 503"):
                execute(worker, claim(store, "plan"))
            # clear the scheduled delay to simulate the clock advancing
            for node in store.list_node_runs(receipt.run_id):
                if node.next_attempt_at is not None:
                    with store.engine.begin() as connection:
                        import sqlalchemy as sa
                        from anchor.state import schema
                        connection.execute(sa.update(schema.node_runs).where(
                            schema.node_runs.c.id == str(node.id)).values(next_attempt_at=None))
        execute(worker, claim(store, "plan"))
        assert gateway.calls == 3
        plan_nodes = [n for n in store.list_node_runs(receipt.run_id) if n.node_id == "plan"]
        assert [n.status.value for n in sorted(plan_nodes, key=lambda n: n.attempt)] == [
            "failed", "failed", "completed"]
    finally:
        store.close()


def test_transient_error_after_many_cycles_uses_same_recovery_semantics(tmp_path):
    """Matrix 6: recovery semantics do not depend on historical cycle count."""
    class Flaky:
        def __init__(self):
            self.calls = 0

        async def generate(self, **kwargs):
            self.calls += 1
            if self.calls == 1:
                raise RuntimeError("HTTP 504 Gateway Time-out")
            return ModelResponse(text=json.dumps({"manuscript": manuscript(), "thesis": "t"}),
                                 provider="test", model="test")

    store, artifacts, receipt, control = setup(tmp_path)
    try:
        asyncio.run(control.execute_once(worker_id="control"))
        complete(store, artifacts, claim(store, "plan"), {"round": 1})
        asyncio.run(control.execute_once(worker_id="control"))  # coverage seed
        complete(store, artifacts, claim(store, "gather"), ledger())
        asyncio.run(control.execute_once(worker_id="control"))  # coverage -> write
        # several completed business cycles first
        for round_number in range(3):
            complete(store, artifacts, claim(store, "write"),
                     {"manuscript": manuscript() + str(round_number), "thesis": "t"})
            complete(store, artifacts, claim(store, "review"),
                     {"verdict": "revise", "target": "manuscript", "issues": []})
            asyncio.run(control.execute_once(worker_id="control"))
        gateway = Flaky()
        worker = agent_worker(store, artifacts, gateway, role="writer", max_retries=2)
        worker.retry_backoff_seconds = (0,)
        with pytest.raises(RuntimeError, match="HTTP 504"):
            execute(worker, claim(store, "write"))
        attempts = sorted((n for n in store.list_node_runs(receipt.run_id)
                           if n.node_id == "write"), key=lambda n: n.attempt)
        assert attempts[-2].last_error_class == "transient_http"
        assert attempts[-1].status.value == "ready"
        execute(worker, claim(store, "write"))
        assert store.get_run(receipt.run_id).status.value == "running"
    finally:
        store.close()


def test_authentication_error_is_never_retried_even_with_budget(tmp_path):
    """Matrix 7: 401/403 are configuration faults, not transient retries."""
    class Unauthorized:
        async def generate(self, **kwargs):
            raise RuntimeError("HTTP 401 unauthorized")

    store, artifacts, receipt, control = setup(tmp_path)
    try:
        asyncio.run(control.execute_once(worker_id="control"))
        worker = agent_worker(store, artifacts, Unauthorized(), max_retries=3)
        with pytest.raises(RuntimeError, match="HTTP 401"):
            execute(worker, claim(store, "plan"))
        plan_nodes = [n for n in store.list_node_runs(receipt.run_id) if n.node_id == "plan"]
        assert len(plan_nodes) == 1  # no new attempt was queued
        assert plan_nodes[0].status.value == "failed"
        assert store.get_run(receipt.run_id).status.value == "failed"
    finally:
        store.close()


def test_stop_wins_over_a_scheduled_retry(tmp_path):
    """Matrix 12: operator stop fences a pending retry; no new work is accepted."""
    class Flaky:
        async def generate(self, **kwargs):
            raise RuntimeError("HTTP 503 Service Unavailable")

    store, artifacts, receipt, control = setup(tmp_path)
    try:
        asyncio.run(control.execute_once(worker_id="control"))
        worker = agent_worker(store, artifacts, Flaky(), max_retries=3)
        worker.retry_backoff_seconds = (3600.0,)
        with pytest.raises(RuntimeError, match="HTTP 503"):
            execute(worker, claim(store, "plan"))
        fresh = max((n for n in store.list_node_runs(receipt.run_id) if n.node_id == "plan"),
                    key=lambda n: n.attempt)
        assert fresh.next_attempt_at is not None
        store.stop_run(receipt.run_id, reason="operator stop beats retry")
        assert store.get_run(receipt.run_id).status.value == "cancelled"
        assert store.claim_ready_agent_node("agent", uuid4()) is None
    finally:
        store.close()
