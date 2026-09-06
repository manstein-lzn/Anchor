import asyncio
import os
import sqlite3
import subprocess
import sys
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from threading import Barrier
from uuid import uuid4

import pytest

sa = pytest.importorskip("sqlalchemy")

from anchor.domain.admission import RunRequest
from anchor.domain.context import input_hash
from anchor.domain.graph import GraphDefinition, GraphEdge, GraphNode, GraphVersion, Trigger
from anchor.domain.models import NodeRun, VerificationRecord
from anchor.domain.operations import OperationStatus, ToolOperation
from anchor.runtime.dispatch import dispatch_pending
from anchor.runtime.receiver import DurableExecutionReceiver
from anchor.state import AdmissionConflict, ConcurrencyConflict, GraphVersionConflict, OperationConflict
from anchor.state import schema as s
from anchor.state.relational import RelationalStateStore
from conftest import make_store, store_url


@pytest.fixture
def seeded(tmp_path):
    store = make_store(tmp_path)
    definition = GraphDefinition(
        graph_id="review", name="Review",
        nodes=[GraphNode(id="research", type="agent", name="Research", agent_ref="research-v1"),
               GraphNode(id="verify", type="verifier", name="Verify", verifier_ref="verify-v1")],
        edges=[GraphEdge(source="research", target="verify")],
    )
    version = store.publish_graph(GraphVersion.publish(definition, 1))
    trigger = store.create_trigger(Trigger(graph_version_id=version.graph_version_id, type="manual"))
    request = RunRequest(trigger_id=trigger.id, idempotency_key="occurrence-1", objective="Review report",
                         inputs={"artifact": "report-1", "options": {"citations": True}})
    yield store, version, request
    store.close()


def counts(store):
    tables = (s.tasks, s.runs, s.node_runs, s.events, s.run_outbox, s.run_admissions)
    with store.engine.connect() as connection:
        return {table.name: connection.scalar(sa.select(sa.func.count()).select_from(table))
                for table in tables}


def passed_verification(lease, snapshot, evidence_ref):
    return VerificationRecord(
        claim_id=lease.claim_id,
        run_id=lease.run_id,
        node_run_id=lease.node_run_id,
        node_id=lease.node_id,
        verifier_ref="verify-v1",
        verifier_version="test-v1",
        adapter="deterministic_test",
        adapter_version="v1",
        verdict="passed",
        reason="test evidence passed",
        evidence_ref=evidence_ref,
        verified_context_hash=input_hash(snapshot),
    )


def test_admission_is_atomic_and_idempotent(seeded):
    store, version, request = seeded
    receipt = store.admit_run(request)
    assert store.admit_run(request) == receipt
    assert store.get_run(receipt.run_id).graph_version_id == version.graph_version_id
    assert store.get_run(receipt.run_id).last_event_sequence == 1
    assert [node.node_id for node in store.list_node_runs(receipt.run_id)] == ["research", "verify"]
    assert store.pending_dispatches()[0].inputs == request.inputs
    assert counts(store) == dict(tasks=1, runs=1, node_runs=2, events=2, run_outbox=1, run_admissions=1)


def test_same_key_different_content_fails_without_extra_writes(seeded):
    store, _, request = seeded
    store.admit_run(request)
    before = counts(store)
    with pytest.raises(AdmissionConflict):
        store.admit_run(request.model_copy(update={"objective": "Different work"}))
    assert counts(store) == before


def test_json_key_order_does_not_change_request_identity(seeded):
    store, _, request = seeded
    receipt = store.admit_run(request)
    reordered = request.model_copy(update={"inputs": {"options": {"citations": True}, "artifact": "report-1"}})
    assert store.admit_run(reordered) == receipt


def test_separate_occurrences_create_separate_runs(seeded):
    store, _, request = seeded
    first = store.admit_run(request)
    second = store.admit_run(request.model_copy(update={"idempotency_key": "occurrence-2"}))
    assert first.run_id != second.run_id


def test_missing_or_disabled_trigger_leaves_no_partial_work(seeded):
    store, version, request = seeded
    with pytest.raises(KeyError):
        store.admit_run(request.model_copy(update={"trigger_id": uuid4()}))
    disabled = store.create_trigger(Trigger(graph_version_id=version.graph_version_id, type="manual", enabled=False))
    with pytest.raises(ValueError, match="disabled"):
        store.admit_run(request.model_copy(update={"trigger_id": disabled.id}))
    assert not any(counts(store).values())


def test_outbox_failure_rolls_back_nested_task_run_and_node_writes(seeded, monkeypatch):
    store, _, request = seeded
    def fail(connection, message):
        raise RuntimeError("simulated outbox failure")
    with monkeypatch.context() as patch:
        patch.setattr(store, "_enqueue_dispatch", fail)
        with pytest.raises(RuntimeError):
            store.admit_run(request)
    assert not any(counts(store).values())
    store.admit_run(request)
    assert counts(store)["runs"] == 1


@pytest.mark.parametrize("committed", [False, True])
def test_process_exit_before_commit_or_after_lost_response(seeded, committed):
    store, _, request = seeded
    script = """
import os, sys
from anchor.state.relational import RelationalStateStore
from anchor.domain.admission import RunRequest
store = RelationalStateStore(sys.argv[1])
if sys.argv[3] == 'False':
    def crash(connection, message):
        os._exit(73)
    store._enqueue_dispatch = crash
store.admit_run(RunRequest.model_validate_json(sys.argv[2]))
os._exit(73)
"""
    env = dict(os.environ, PYTHONPATH=str(Path(__file__).resolve().parents[1] / "src"))
    child = subprocess.run([sys.executable, "-c", script, store_url(store), request.model_dump_json(), str(committed)],
                           env=env, timeout=20, capture_output=True, text=True)
    assert child.returncode == 73, child.stderr
    assert counts(store)["runs"] == int(committed)
    receipt = store.admit_run(request)
    with_reopen = RelationalStateStore(store_url(store))
    try:
        assert with_reopen.admit_run(request) == receipt
        assert counts(store)["runs"] == 1
        assert len(with_reopen.pending_dispatches()) == 1
    finally:
        with_reopen.close()


def test_two_connections_racing_same_occurrence_return_one_run(seeded):
    store, _, request = seeded
    other = RelationalStateStore(store_url(store))
    barrier = Barrier(2)
    def admit(connection):
        barrier.wait(timeout=10)
        return connection.admit_run(request)
    try:
        with ThreadPoolExecutor(max_workers=2) as pool:
            receipts = list(pool.map(admit, [store, other]))
        assert receipts[0] == receipts[1]
        assert counts(store)["runs"] == 1
    finally:
        other.close()


def test_new_graph_version_does_not_change_existing_run(seeded):
    store, first, request = seeded
    receipt = store.admit_run(request)
    changed = first.definition.model_copy(deep=True)
    changed.nodes[0].agent_ref = "research-v2"
    second = store.publish_graph(GraphVersion.publish(changed, 2))
    assert second.graph_version_id != first.graph_version_id
    assert store.get_run(receipt.run_id).graph_version_id == first.graph_version_id
    assert store.get_graph_version(first.graph_version_id).definition.nodes[0].agent_ref == "research-v1"


def test_mutated_published_dto_is_rejected(seeded):
    store, version, _ = seeded
    version.definition.nodes[0].agent_ref = "tampered"
    with pytest.raises(GraphVersionConflict):
        store.publish_graph(version)
    assert store.get_graph_version(version.graph_version_id).definition.nodes[0].agent_ref == "research-v1"


def test_node_must_belong_to_pinned_graph(seeded):
    store, _, request = seeded
    receipt = store.admit_run(request)
    with pytest.raises(ValueError, match="node does not belong"):
        store.create_node_run(NodeRun(run_id=receipt.run_id, node_id="unknown"))


def test_dispatch_ack_loss_redelivers_same_message(seeded, tmp_path, monkeypatch):
    store, _, request = seeded
    receipt = store.admit_run(request)
    target_path = tmp_path / "receiver.sqlite"

    class DurableTestTarget:
        async def accept(self, message):
            with sqlite3.connect(target_path) as connection:
                connection.execute("CREATE TABLE IF NOT EXISTS accepted (id TEXT PRIMARY KEY)")
                connection.execute("INSERT OR IGNORE INTO accepted VALUES (?)", (str(message.message_id),))

    def ack_lost(message_id):
        raise ConnectionError("accepted but acknowledgement lost")
    with monkeypatch.context() as patch:
        patch.setattr(store, "acknowledge_dispatch", ack_lost)
        with pytest.raises(ConnectionError):
            asyncio.run(dispatch_pending(store, DurableTestTarget()))
    assert store.pending_dispatches()[0].message_id == receipt.message_id
    assert asyncio.run(dispatch_pending(store, DurableTestTarget())) == 1
    assert store.pending_dispatches() == []
    store.acknowledge_dispatch(receipt.message_id)
    with sqlite3.connect(target_path) as connection:
        assert connection.execute("SELECT count(*) FROM accepted").fetchone()[0] == 1


def test_transport_failure_does_not_acknowledge_message(seeded):
    store, _, request = seeded
    store.admit_run(request)
    class Offline:
        async def accept(self, message):
            raise ConnectionError("offline")
    with pytest.raises(ConnectionError):
        asyncio.run(dispatch_pending(store, Offline()))
    assert len(store.pending_dispatches()) == 1


def test_receiver_accepts_dispatch_once_and_replay_is_safe(seeded):
    store, _, request = seeded
    receipt = store.admit_run(request)
    message = store.pending_dispatches()[0]
    receiver = DurableExecutionReceiver(store)
    asyncio.run(receiver.accept(message))
    first = store.get_run(receipt.run_id)
    assert first is not None and first.status.value == "queued" and first.revision == 1
    assert [event["event_type"] for event in store.list_events(receipt.run_id)] == [
        "run.requested", "run.dispatch_accepted", "node.ready"
    ]
    nodes = {node.node_id: node for node in store.list_node_runs(receipt.run_id)}
    assert nodes["research"].status.value == "ready"
    assert nodes["verify"].status.value == "pending"
    assert store.accepted_dispatches() == [message]
    asyncio.run(receiver.accept(message))
    second = store.get_run(receipt.run_id)
    assert second is not None and second.revision == first.revision
    assert len(store.list_events(receipt.run_id)) == 3
    assert store.get_task(receipt.task_id).status.value == "ready"


def test_receiver_rejects_message_for_a_different_pinned_run(seeded):
    store, _, request = seeded
    store.admit_run(request)
    message = store.pending_dispatches()[0]
    from anchor.domain.admission import RunDispatch
    tampered = RunDispatch.model_validate(message.model_dump(mode="json") | {"task_id": str(uuid4())})
    with pytest.raises(ValueError, match="does not match"):
        asyncio.run(DurableExecutionReceiver(store).accept(tampered))


def test_runtime_heartbeat_reports_recent_receiver_only(seeded):
    store, _, _ = seeded
    assert not store.runtime_connected("execution_receiver")
    store.record_runtime_heartbeat("execution_receiver", uuid4())
    assert store.runtime_connected("execution_receiver")
    with pytest.raises(ValueError, match="positive"):
        store.runtime_connected("execution_receiver", within_seconds=0)


def test_ready_node_claim_is_stable_and_does_not_steal(seeded):
    store, _, request = seeded
    receipt = store.admit_run(request)
    asyncio.run(DurableExecutionReceiver(store).accept(store.pending_dispatches()[0]))
    claim_id = uuid4()
    lease = store.claim_ready_node("worker-a", claim_id)
    assert lease is not None and lease.node_id == "research" and lease.claim_id == claim_id
    assert store.claim_ready_node("worker-a", claim_id) == lease
    assert store.claim_ready_node("worker-b", uuid4()) is None
    with pytest.raises(ConcurrencyConflict, match="another worker"):
        store.claim_ready_node("worker-b", claim_id)
    heartbeat = store.heartbeat_node_lease(claim_id, "worker-a")
    assert heartbeat.heartbeat_at >= lease.heartbeat_at
    with pytest.raises(ConcurrencyConflict, match="another worker"):
        store.heartbeat_node_lease(claim_id, "worker-b")
    run = store.get_run(receipt.run_id)
    assert run is not None and run.status.value == "running" and run.revision == 2
    assert {node.node_id: node.status.value for node in store.list_node_runs(receipt.run_id)} == {
        "research": "running", "verify": "pending"
    }
    assert [event["event_type"] for event in store.list_events(receipt.run_id)] == [
        "run.requested", "run.dispatch_accepted", "node.ready", "node.claimed", "run.running"
    ]


def test_interrupted_agent_lease_can_be_recovered_and_reclaimed(seeded):
    store, _, request = seeded
    receipt = store.admit_run(request)
    asyncio.run(DurableExecutionReceiver(store).accept(store.pending_dispatches()[0]))
    lease = store.claim_ready_node("worker-a", uuid4())
    recovered = store.recover_model_lease(lease.claim_id, reason="worker_interrupted")
    assert recovered.status.value == "ready" and recovered.error_code == "worker_interrupted"
    reclaimed = store.claim_ready_node("worker-b", uuid4())
    assert reclaimed is not None and reclaimed.node_run_id == lease.node_run_id
    assert "node.recovered" in [event["event_type"] for event in store.list_events(receipt.run_id)]


def test_non_agent_lease_cannot_use_model_recovery(seeded):
    store, _, request = seeded
    store.admit_run(request)
    asyncio.run(DurableExecutionReceiver(store).accept(store.pending_dispatches()[0]))
    first = store.claim_ready_node("worker-a", uuid4())
    store.complete_node_and_propagate(first.claim_id, "worker-a", output_ref="artifact://sha256/a")
    second = store.claim_ready_node("worker-a", uuid4())
    with pytest.raises(ConcurrencyConflict, match="Agent"):
        store.recover_model_lease(second.claim_id, reason="interrupted")


def test_tool_operation_ledger_is_replay_safe_and_unknown_is_terminal(seeded):
    store, _, request = seeded
    receipt = store.admit_run(request)
    asyncio.run(DurableExecutionReceiver(store).accept(store.pending_dispatches()[0]))
    lease = store.claim_ready_node("worker-a", uuid4())
    assert lease is not None
    operation = ToolOperation.register(operation_id=uuid4(), claim_id=lease.claim_id,
        node_run_id=lease.node_run_id, run_id=lease.run_id, tool_ref="search.query",
        arguments={"query": "durable agents", "page": 1})
    assert store.register_tool_operation(operation) == operation
    assert store.register_tool_operation(operation) == operation
    altered = operation.model_copy(update={"arguments": {"query": "different"}})
    with pytest.raises(ValueError, match="hash"):
        store.register_tool_operation(altered)
    running = store.start_tool_operation(operation.operation_id, lease.claim_id)
    assert running.status is OperationStatus.RUNNING
    assert store.start_tool_operation(operation.operation_id, lease.claim_id) == running
    unknown = store.finish_tool_operation(operation.operation_id, lease.claim_id,
        status=OperationStatus.OUTCOME_UNKNOWN, error_code="worker_disconnected")
    assert unknown.status is OperationStatus.OUTCOME_UNKNOWN
    assert store.finish_tool_operation(operation.operation_id, lease.claim_id,
        status=OperationStatus.OUTCOME_UNKNOWN, error_code="worker_disconnected") == unknown
    with pytest.raises(OperationConflict, match="cannot be started"):
        store.start_tool_operation(operation.operation_id, lease.claim_id)
    with pytest.raises(OperationConflict, match="conflicts"):
        store.finish_tool_operation(operation.operation_id, lease.claim_id,
            status=OperationStatus.OUTCOME_UNKNOWN, error_code="different")
    reconciled = store.reconcile_tool_operation(operation.operation_id, status=OperationStatus.SUCCEEDED,
        reconciliation_ref="provider://query/receipt-1", result_ref="artifact://search/result-1")
    assert reconciled.status is OperationStatus.SUCCEEDED
    assert store.reconcile_tool_operation(operation.operation_id, status=OperationStatus.SUCCEEDED,
        reconciliation_ref="provider://query/receipt-1", result_ref="artifact://search/result-1") == reconciled
    assert store.list_tool_operations(receipt.run_id) == [reconciled]
    with pytest.raises(OperationConflict, match="evidence"):
        store.reconcile_tool_operation(operation.operation_id, status=OperationStatus.FAILED,
            reconciliation_ref="provider://query/other", error_code="not_found")
    assert [event["event_type"] for event in store.list_events(receipt.run_id)][-4:] == [
        "operation.registered", "operation.running", "operation.outcome_unknown", "operation.reconciled"
    ]


def test_complete_node_propagates_and_completes_terminal_graph(seeded):
    store, _, request = seeded
    receipt = store.admit_run(request)
    asyncio.run(DurableExecutionReceiver(store).accept(store.pending_dispatches()[0]))
    lease = store.claim_ready_node("worker-a", uuid4())
    assert lease is not None
    next_nodes = store.complete_node_and_propagate(lease.claim_id, "worker-a", output_ref="artifact://sha256/research",
                                                      node_input_hash="hash-research")
    assert [node.node_id for node in next_nodes] == ["verify"]
    assert next_nodes[0].status.value == "ready"
    assert store.get_run(receipt.run_id).status.value == "running"
    completed = {node.node_id: node for node in store.list_node_runs(receipt.run_id)}
    assert completed["research"].input_hash == "hash-research"
    completed_events = [event for event in store.list_events(receipt.run_id)
                        if event["event_type"] == "node.completed"]
    assert completed_events[0]["payload"]["input_hash"] == "hash-research"
    second = store.claim_ready_node("worker-a", uuid4())
    assert second is not None and second.node_id == "verify"
    snapshot = {"inputs": {"artifact": "report-1"}}
    evidence_ref = "artifact://sha256/verify"
    store.complete_node_and_propagate(
        second.claim_id,
        "worker-a",
        output_ref=evidence_ref,
        input_snapshot=snapshot,
        verification=passed_verification(second, snapshot, evidence_ref),
    )
    assert store.get_run(receipt.run_id).status.value == "completed"
    assert store.get_task(receipt.task_id).status.value == "completed"
    assert [event["event_type"] for event in store.list_events(receipt.task_id)][-1] == "task.completed"


def test_complete_node_requires_lease_owner(seeded):
    store, _, request = seeded
    store.admit_run(request)
    asyncio.run(DurableExecutionReceiver(store).accept(store.pending_dispatches()[0]))
    lease = store.claim_ready_node("worker-a", uuid4())
    with pytest.raises(ConcurrencyConflict, match="owned"):
        store.complete_node_and_propagate(lease.claim_id, "worker-b", output_ref="artifact://sha256/x")


def test_fail_node_marks_run_failed_and_is_idempotent(seeded):
    store, _, request = seeded
    receipt = store.admit_run(request)
    asyncio.run(DurableExecutionReceiver(store).accept(store.pending_dispatches()[0]))
    lease = store.claim_ready_node("worker-a", uuid4())
    failed = store.fail_node_and_propagate(lease.claim_id, "worker-a", error_code="model_unavailable", phase="model")
    assert failed.status.value == "failed"
    assert failed.error_code == "model_unavailable"
    assert store.get_run(receipt.run_id).status.value == "failed"
    assert store.get_task(receipt.task_id).status.value == "failed"
    assert store.fail_node_and_propagate(lease.claim_id, "worker-a", error_code="model_unavailable", phase="model") == failed
    with pytest.raises(ConcurrencyConflict, match="different error"):
        store.fail_node_and_propagate(lease.claim_id, "worker-a", error_code="other")
    assert [event["event_type"] for event in store.list_events(receipt.run_id)][-2:] == ["node.failed", "run.failed"]
    assert [event["event_type"] for event in store.list_events(receipt.task_id)][-1] == "task.failed"


def test_failed_run_never_revives_or_opens_downstream(seeded):
    store, _, request = seeded
    definition = GraphDefinition(
        graph_id="parallel-failure", name="Parallel failure",
        nodes=[
            GraphNode(id="start", type="agent", name="Start", agent_ref="start-v1"),
            GraphNode(id="left", type="agent", name="Left", agent_ref="left-v1"),
            GraphNode(id="right", type="agent", name="Right", agent_ref="right-v1"),
            GraphNode(id="join", type="agent", name="Join", agent_ref="join-v1"),
        ],
        edges=[
            GraphEdge(source="start", target="left"),
            GraphEdge(source="start", target="right"),
            GraphEdge(source="left", target="join"),
            GraphEdge(source="right", target="join"),
        ],
    )
    version = store.publish_graph(GraphVersion.publish(definition, 2))
    trigger = store.create_trigger(Trigger(graph_version_id=version.graph_version_id, type="manual"))
    receipt = store.admit_run(request.model_copy(update={"trigger_id": trigger.id, "idempotency_key": "parallel-failure"}))
    asyncio.run(DurableExecutionReceiver(store).accept(store.pending_dispatches()[0]))
    first = store.claim_ready_node("worker-a", uuid4())
    assert first is not None and first.node_id == "start"
    store.complete_node_and_propagate(first.claim_id, "worker-a", output_ref="artifact://start")
    left = store.claim_ready_node("worker-a", uuid4())
    right = store.claim_ready_node("worker-b", uuid4())
    assert left is not None and right is not None
    store.fail_node_and_propagate(left.claim_id, "worker-a", error_code="verification_failed", phase="verify")
    store.complete_node_and_propagate(right.claim_id, "worker-b", output_ref="artifact://right")
    assert store.get_run(receipt.run_id).status.value == "failed"
    nodes = {node.node_id: node for node in store.list_node_runs(receipt.run_id)}
    assert nodes["left"].status.value == "failed"
    assert nodes["right"].status.value == "completed"
    assert nodes["join"].status.value == "pending"
    assert "run.completed" not in [event["event_type"] for event in store.list_events(receipt.run_id)]


def test_agent_claim_skips_ready_nodes_without_agent_executor(seeded):
    store, _, request = seeded
    definition = GraphDefinition(
        graph_id="agent-claim-filter", name="Agent claim filter",
        nodes=[
            GraphNode(id="start", type="agent", name="Start", agent_ref="start-v1"),
            GraphNode(id="verify", type="verifier", name="Verify", verifier_ref="verify-v1"),
            GraphNode(id="next", type="agent", name="Next", agent_ref="next-v1"),
        ],
        edges=[GraphEdge(source="start", target="verify"), GraphEdge(source="start", target="next")],
    )
    version = store.publish_graph(GraphVersion.publish(definition, 2))
    trigger = store.create_trigger(Trigger(graph_version_id=version.graph_version_id, type="manual"))
    receipt = store.admit_run(request.model_copy(update={"trigger_id": trigger.id, "idempotency_key": "agent-filter"}))
    asyncio.run(DurableExecutionReceiver(store).accept(store.pending_dispatches()[0]))
    first = store.claim_ready_agent_node("worker", uuid4())
    assert first is not None and first.node_id == "start"
    store.complete_node_and_propagate(first.claim_id, "worker", output_ref="artifact://start")
    agent = store.claim_ready_agent_node("worker", uuid4())
    assert agent is not None and agent.node_id == "next"
    verifier = store.claim_ready_node("worker", uuid4())
    assert verifier is not None and verifier.node_id == "verify"


def test_context_snapshot_is_atomic_replayable_and_reexecution_safe(seeded):
    store, _, request = seeded
    receipt = store.admit_run(request)
    asyncio.run(DurableExecutionReceiver(store).accept(store.pending_dispatches()[0]))
    lease = store.claim_ready_node("worker-a", uuid4())
    snapshot = {"inputs": {"topic": "durable"}, "memory": ["verified"]}
    store.complete_node_and_propagate(lease.claim_id, "worker-a", output_ref="artifact://sha256/one",
                                      input_snapshot=snapshot)
    node = next(item for item in store.list_node_runs(receipt.run_id) if item.node_id == "research")
    saved = store.get_context_snapshot(node.id)
    assert saved is not None and saved.snapshot == snapshot
    assert saved.input_hash == node.input_hash and saved.generation == 1
    completed_event = next(event for event in store.list_events(receipt.run_id)
                           if event["event_type"] == "node.completed")
    assert completed_event["payload"]["context_generation"] == 1
    assert store.get_run(receipt.run_id).context_generation == 1

    # A downstream execution receives a new immutable generation.
    downstream = store.claim_ready_node("worker-a", uuid4())
    assert downstream is not None and downstream.node_id == "verify"
    second_snapshot = {"prior": "artifact://sha256/one"}
    evidence_ref = "artifact://sha256/two"
    store.complete_node_and_propagate(
        downstream.claim_id,
        "worker-a",
        output_ref=evidence_ref,
        input_snapshot=second_snapshot,
        verification=passed_verification(downstream, second_snapshot, evidence_ref),
    )
    snapshots = store.list_context_snapshots(receipt.run_id)
    assert [item.generation for item in snapshots] == [1, 2]
    assert snapshots[1].snapshot == second_snapshot


def test_context_snapshot_hash_mismatch_rolls_back_completion(seeded):
    store, _, request = seeded
    receipt = store.admit_run(request)
    asyncio.run(DurableExecutionReceiver(store).accept(store.pending_dispatches()[0]))
    lease = store.claim_ready_node("worker-a", uuid4())
    with pytest.raises(ValueError, match="does not match"):
        store.complete_node_and_propagate(lease.claim_id, "worker-a", output_ref="artifact://sha256/x",
                                          node_input_hash="0" * 64, input_snapshot={"value": 1})
    assert store.get_context_snapshot(lease.node_run_id) is None
    assert store.list_node_runs(receipt.run_id)[0].status.value == "running"


def test_conditional_branch_decisions_skip_rejected_path_and_join(seeded):
    store, _, request = seeded
    definition = GraphDefinition(
        graph_id="conditional-join", name="Conditional join",
        nodes=[
            GraphNode(id="route", type="agent", name="Route", agent_ref="route-v1"),
            GraphNode(id="left", type="agent", name="Left", agent_ref="left-v1"),
            GraphNode(id="right", type="agent", name="Right", agent_ref="right-v1"),
            GraphNode(id="join", type="agent", name="Join", agent_ref="join-v1"),
        ],
        edges=[
            GraphEdge(source="route", target="left", condition="output.approved"),
            GraphEdge(source="route", target="right", condition="output.approved == `false`"),
            GraphEdge(source="left", target="join"),
            GraphEdge(source="right", target="join"),
        ],
    )
    version = store.publish_graph(GraphVersion.publish(definition, 1))
    trigger = store.create_trigger(Trigger(graph_version_id=version.graph_version_id, type="manual"))
    receipt = store.admit_run(request.model_copy(update={
        "trigger_id": trigger.id, "idempotency_key": "conditional-join",
    }))
    asyncio.run(DurableExecutionReceiver(store).accept(store.pending_dispatches()[0]))

    route = store.claim_ready_node("worker", uuid4())
    assert route is not None and route.node_id == "route"
    ready = store.complete_node_and_propagate(
        route.claim_id, "worker", output_ref="artifact://route",
        condition_context={"output": {"approved": True}, "inputs": {}},
    )
    assert [item.node_id for item in ready] == ["left"]
    nodes = {item.node_id: item for item in store.list_node_runs(receipt.run_id)}
    assert nodes["right"].status.value == "skipped"
    assert nodes["join"].status.value == "pending"

    left = store.claim_ready_node("worker", uuid4())
    assert left is not None and left.node_id == "left"
    ready = store.complete_node_and_propagate(
        left.claim_id, "worker", output_ref="artifact://left",
    )
    assert [item.node_id for item in ready] == ["join"]
    join = store.claim_ready_node("worker", uuid4())
    assert join is not None and join.node_id == "join"
    store.complete_node_and_propagate(join.claim_id, "worker", output_ref="artifact://join")

    decisions = store.list_edge_decisions(receipt.run_id)
    assert [(item.edge_index, item.selected, item.reason.value) for item in decisions] == [
        (0, True, "condition_true"),
        (1, False, "condition_false"),
        (2, True, "unconditional"),
        (3, False, "upstream_skipped"),
    ]
    assert decisions[0].evaluation_context_hash == decisions[1].evaluation_context_hash
    assert decisions[0].evidence_ref == "artifact://route"
    assert store.get_run(receipt.run_id).status.value == "completed"
    assert store.get_task(receipt.task_id).status.value == "completed"
    events = store.list_events(receipt.run_id)
    assert any(item["event_type"] == "node.skipped" for item in events)
    assert store.get_run(receipt.run_id).last_event_sequence == events[-1]["sequence"]


def test_non_boolean_condition_result_rolls_back_completion(seeded):
    store, _, request = seeded
    definition = GraphDefinition(
        graph_id="strict-condition", name="Strict condition",
        nodes=[
            GraphNode(id="route", type="agent", name="Route", agent_ref="route-v1"),
            GraphNode(id="next", type="agent", name="Next", agent_ref="next-v1"),
        ],
        edges=[GraphEdge(source="route", target="next", condition="output.result")],
    )
    version = store.publish_graph(GraphVersion.publish(definition, 1))
    trigger = store.create_trigger(Trigger(graph_version_id=version.graph_version_id, type="manual"))
    receipt = store.admit_run(request.model_copy(update={
        "trigger_id": trigger.id, "idempotency_key": "strict-condition",
    }))
    asyncio.run(DurableExecutionReceiver(store).accept(store.pending_dispatches()[0]))
    lease = store.claim_ready_node("worker", uuid4())
    before = store.list_events(receipt.run_id)
    with pytest.raises(ValueError, match="must evaluate to a boolean"):
        store.complete_node_and_propagate(
            lease.claim_id, "worker", output_ref="artifact://route",
            condition_context={"output": {"result": "yes"}, "inputs": {}},
        )
    nodes = {item.node_id: item for item in store.list_node_runs(receipt.run_id)}
    assert nodes["route"].status.value == "running"
    assert store.list_edge_decisions(receipt.run_id) == []
    assert store.list_events(receipt.run_id) == before
