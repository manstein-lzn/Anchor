"""A branch failure must end every node the run will never reach.

The defect this covers was measured, not hypothesised: failed runs held 76 nodes in
`pending` with no terminal state and nothing recording why. From outside that is
indistinguishable from a run still waiting for a worker, which is the worst thing a stalled
run can look like.

Four timings, because a sibling can be anywhere when its neighbour fails:

  not yet started   → must be ended, with a reason
  in flight         → its lease is released, which fences its result
  already finished  → untouched; it is legitimate evidence
  outcome unknown   → nothing may be fabricated about it

Plus the two properties that make the whole thing trustworthy: a duplicate failure event
must not fan out twice, and a failed run must not be claimable again.
"""

from __future__ import annotations

import asyncio
from uuid import uuid4

import pytest

from anchor.domain.admission import RunRequest
from anchor.domain.graph import GraphDefinition, GraphVersion, Trigger
from anchor.runtime.artifacts import LocalArtifactStore
from anchor.runtime.behaviors import BehaviorRegistry
from anchor.runtime.control_worker import ControlNodeWorker
from anchor.runtime.dispatch import dispatch_pending
from anchor.runtime.receiver import DurableExecutionReceiver
from anchor.runtime.sinks import ArtifactCheckpointSink
from anchor.state.errors import FencedAttempt

from conftest import make_store


#: A fork and a join. `a` and `b` are siblings; `tail` and `end` are downstream of `a`, so a
#: failure in `a` leaves them unreachable.
FORK = {
    "graph_id": "fork",
    "name": "Fork",
    "entry_node_id": "start",
    "nodes": [
        {"id": "start", "type": "artifact", "name": "Start"},
        {"id": "a", "type": "agent", "name": "A", "agent_ref": "agents.x"},
        {"id": "b", "type": "agent", "name": "B", "agent_ref": "agents.x"},
        {"id": "tail", "type": "agent", "name": "Tail", "agent_ref": "agents.x"},
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


def forked(tmp_path, name="fork.sqlite"):
    """A run whose entry has completed, so `a` and `b` are both ready."""
    store = make_store(tmp_path, name)
    artifacts = LocalArtifactStore(tmp_path / "artifacts")
    definition = GraphDefinition.model_validate(FORK)
    version = store.publish_graph(GraphVersion.publish(definition, 1))
    trigger = store.create_trigger(Trigger(graph_version_id=version.graph_version_id,
                                           type="manual"))
    receipt = store.admit_run(RunRequest(trigger_id=trigger.id,
                                         idempotency_key=f"fork-{uuid4().hex}",
                                         objective="fork", inputs={}))
    asyncio.run(dispatch_pending(store, DurableExecutionReceiver(store)))
    control = ControlNodeWorker(store, artifacts,
                                ArtifactCheckpointSink(store, artifacts, "control"),
                                behaviors=BehaviorRegistry())
    asyncio.run(control.execute_once(worker_id="control"))  # completes `start`
    return store, artifacts, receipt, control


def claim(store, node_id):
    lease = store.claim_ready_agent_node("agent", uuid4())
    assert lease is not None and lease.node_id == node_id, f"expected {node_id}"
    return lease


def states(store, run_id) -> dict[str, str]:
    return {item.node_id: item.status.value for item in store.list_node_runs(run_id)}


def fail(store, lease, code="branch_failed"):
    return store.fail_node_and_propagate(lease.claim_id, "agent", error_code=code,
                                         phase="agent")


def test_a_sibling_that_never_started_is_ended_with_a_reason(tmp_path):
    """The measured defect: these used to sit `pending` forever."""
    store, _artifacts, receipt, _control = forked(tmp_path)
    try:
        a = claim(store, "a")
        claim(store, "b")
        fail(store, a)

        assert states(store, receipt.run_id) == {
            "start": "completed",
            "a": "failed",
            "b": "cancelled",
            "tail": "cancelled",
            "end": "cancelled",
        }
        reasons = {item.node_id: item.error_code for item in store.list_node_runs(receipt.run_id)}
        assert reasons["tail"] == "run_failed", \
            "an abandoned node must record why, or it is indistinguishable from waiting"
        assert reasons["a"] == "branch_failed", "the cause keeps its own reason"
    finally:
        store.close()


def test_an_in_flight_sibling_is_fenced(tmp_path):
    """Its lease is released, so it can neither keep spending nor write a result."""
    store, _artifacts, receipt, _control = forked(tmp_path)
    try:
        a = claim(store, "a")
        b = claim(store, "b")  # b is in flight, holding a live lease
        assert len(store.list_active_leases()) == 2
        fail(store, a)

        assert store.list_active_leases() == [], "the sibling's lease must not survive"
        assert next(item for item in store.list_node_runs(receipt.run_id)
                    if item.node_id == "b").status.value == "cancelled"
        # Completion requires an unreleased lease, so the fenced worker cannot commit a
        # result into a run that has already failed. Raised as FencedAttempt rather than a
        # conflict, so the worker can tell "the run ended under me" from "I am confused".
        with pytest.raises(FencedAttempt, match="was released while its worker held it"):
            store.complete_node_and_propagate(b.claim_id, "agent", output_ref="artifact://x")
    finally:
        store.close()


def test_a_completed_sibling_is_untouched(tmp_path):
    """It is legitimate evidence of work that really happened."""
    store, artifacts, receipt, _control = forked(tmp_path)
    try:
        a = claim(store, "a")
        b = claim(store, "b")
        output = artifacts.put_text("{}")
        store.complete_node_and_propagate(b.claim_id, "agent", output_ref=output)
        fail(store, a)

        got = states(store, receipt.run_id)
        assert got["b"] == "completed", "a finished sibling is evidence, not debris"
        assert got["a"] == "failed"
        # `end` waits only on `b`, which succeeded, and on `tail`, which never ran. The run
        # is failed, so neither downstream opens; `end` is ended rather than left ready.
        assert got["end"] == "cancelled", got
        assert got["tail"] == "cancelled"
    finally:
        store.close()


def test_the_operation_ledger_is_not_touched(tmp_path):
    """An unknown outcome stays unknown: the fan-out records no verdict about it."""
    store, _artifacts, receipt, _control = forked(tmp_path)
    try:
        a = claim(store, "a")
        claim(store, "b")
        before = store.list_tool_operations(receipt.run_id)
        fail(store, a)
        assert store.list_tool_operations(receipt.run_id) == before, \
            "the fan-out must not fabricate an outcome for a sibling's side effects"
    finally:
        store.close()


def test_a_duplicate_failure_does_not_fan_out_twice(tmp_path):
    """At-least-once delivery means the same failure arrives more than once."""
    store, _artifacts, receipt, _control = forked(tmp_path)
    try:
        a = claim(store, "a")
        claim(store, "b")
        fail(store, a)
        events_after_first = len(store.list_events(receipt.run_id))
        snapshot = states(store, receipt.run_id)

        fail(store, a)  # duplicate delivery of the same failure
        assert states(store, receipt.run_id) == snapshot
        assert len(store.list_events(receipt.run_id)) == events_after_first, \
            "a replayed failure must not append a second run.failed"
    finally:
        store.close()


def test_the_failure_event_records_how_many_nodes_it_ended(tmp_path):
    """So the count is auditable without inventing a per-node event."""
    store, _artifacts, receipt, _control = forked(tmp_path)
    try:
        a = claim(store, "a")
        claim(store, "b")
        fail(store, a)
        event = next(item for item in store.list_events(receipt.run_id)
                     if item["event_type"] == "run.failed")
        assert event["payload"]["abandoned_nodes"] == 3, "b, tail and end"
        assert event["payload"]["error_code"] == "branch_failed"
    finally:
        store.close()


def test_a_failed_run_cannot_be_claimed_again(tmp_path):
    """The regression the plan calls out: no sibling reopens downstream."""
    store, _artifacts, receipt, _control = forked(tmp_path)
    try:
        a = claim(store, "a")
        claim(store, "b")
        fail(store, a)
        assert store.get_run(receipt.run_id).status.value == "failed"
        assert store.claim_ready_agent_node("agent", uuid4()) is None, \
            "a failed run must not hand out work"
        assert store.claim_ready_control_node("control", uuid4()) is None
    finally:
        store.close()


def test_the_worker_stops_quietly_when_its_result_is_fenced(tmp_path):
    """A run that ended under a worker is not a fault, and the journal should not say it is.

    Without this the worker would try to fail a node that already has a terminal state,
    raise a second time, and leave a traceback describing a run that behaved correctly.
    """
    store, artifacts, receipt, _control = forked(tmp_path)
    try:
        lease = claim(store, "a")
        fail(store, claim(store, "b"))  # b fails, which fails the run and ends a

        from anchor.runtime.capabilities import AgentCapability, CapabilityRegistry, ModelProfile
        from anchor.runtime.model_gateway import ModelResponse
        from anchor.runtime.worker import AgentNodeWorker

        class Gateway:
            async def generate(self, **kwargs):
                return ModelResponse(text="{}", provider="test", model="test")

        class FencedSink:
            async def persist_model_result(self, **kwargs):
                raise FencedAttempt("lease was released while its worker held it")

        registry = CapabilityRegistry(
            agents=[AgentCapability(ref="agents.other", model_ref="m")],
            models=[ModelProfile(ref="m", provider="test", model="t", secret_ref="unused")])
        worker = AgentNodeWorker(store, registry, {"m": Gateway()}, FencedSink())
        outcome = asyncio.run(worker.execute_claimed_once(
            worker_id="agent", agent_ref="agents.other", lease=lease, prompt="task",
            heartbeat_interval=0.01, input_snapshot={}))
        assert outcome.response.model == "fenced", \
            "the worker must record that it was fenced, not raise"
        # And it must not have written a second failure over the fan-out's decision.
        assert next(item for item in store.list_node_runs(receipt.run_id)
                    if item.node_id == "a").error_code == "run_failed"
    finally:
        store.close()


def test_one_undeliverable_dispatch_does_not_stall_the_rest(tmp_path):
    """A single bad message used to abort the whole batch, forever, with only a log line.

    Everything queued behind it waited on it, so one row nobody could accept could hold up
    every run admitted after it.
    """
    from anchor.runtime.dispatch import dispatch_pending
    from anchor.runtime.receiver import DurableExecutionReceiver

    store = make_store(tmp_path, "dispatch.sqlite")
    try:
        definition = GraphDefinition.model_validate(FORK)
        version = store.publish_graph(GraphVersion.publish(definition, 1))
        trigger = store.create_trigger(Trigger(graph_version_id=version.graph_version_id,
                                               type="manual"))
        runs = [store.admit_run(RunRequest(trigger_id=trigger.id,
                                           idempotency_key=f"d-{index}",
                                           objective="x", inputs={})).run_id
                for index in range(3)]
        pending = store.pending_dispatches(10)
        assert len(pending) == 3
        poisoned = pending[0].message_id

        class PoisonOne(DurableExecutionReceiver):
            async def accept(self, message):
                if message.message_id == poisoned:
                    raise RuntimeError("this graph version cannot be resolved")
                await super().accept(message)

        delivered = asyncio.run(dispatch_pending(store, PoisonOne(store)))
        assert delivered == 2, "the two behind it must still be delivered"
        remaining = [item.message_id for item in store.pending_dispatches(10)]
        assert remaining == [poisoned], "the failed one stays pending for the next pass"

        # The run behind it really did get admitted, rather than merely acknowledged.
        assert store.get_run(runs[1]) is not None
        assert store.get_run(runs[2]) is not None
    finally:
        store.close()


def test_a_stuck_dispatch_leaves_a_durable_trace(tmp_path):
    """Not only a log line: an operator has to be able to see it without grepping journals."""
    from anchor.runtime.dispatch import dispatch_pending
    from anchor.runtime.receiver import DurableExecutionReceiver

    store = make_store(tmp_path, "stuck.sqlite")
    try:
        definition = GraphDefinition.model_validate(FORK)
        version = store.publish_graph(GraphVersion.publish(definition, 1))
        trigger = store.create_trigger(Trigger(graph_version_id=version.graph_version_id,
                                               type="manual"))
        receipt = store.admit_run(RunRequest(trigger_id=trigger.id, idempotency_key="s-1",
                                             objective="x", inputs={}))

        class AlwaysFails(DurableExecutionReceiver):
            async def accept(self, message):
                raise RuntimeError("graph version is missing")

        asyncio.run(dispatch_pending(store, AlwaysFails(store)))
        events = [item for item in store.list_events(receipt.run_id)
                  if item["event_type"] == "run.dispatch_failed"]
        assert len(events) == 1
        assert events[0]["payload"]["error_class"] == "RuntimeError"
        assert "graph version is missing" in events[0]["payload"]["error"]

        # Retrying every pass must not append it again: the fact is recorded once.
        asyncio.run(dispatch_pending(store, AlwaysFails(store)))
        assert len([item for item in store.list_events(receipt.run_id)
                    if item["event_type"] == "run.dispatch_failed"]) == 1
    finally:
        store.close()
