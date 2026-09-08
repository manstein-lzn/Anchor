import asyncio
import os
import subprocess
import sys
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from threading import Barrier
from uuid import uuid4

import pytest

sa = pytest.importorskip("sqlalchemy")
pytest.importorskip("alembic")
from alembic import command
from alembic.config import Config
from sqlalchemy.engine import make_url

from anchor.domain.admission import RunRequest
from anchor.domain.context import input_hash
from anchor.domain.graph import GraphDefinition, GraphEdge, GraphNode, GraphVersion, Trigger
from anchor.domain.models import NodeRun, RunStatus, VerificationRecord
from anchor.domain.operations import OperationStatus, ToolOperation
from anchor.runtime.dispatch import dispatch_pending
from anchor.runtime.receiver import DurableExecutionReceiver
from anchor.state.relational import RelationalStateStore
from anchor.state import schema as s
from anchor.state import AdmissionConflict, ConcurrencyConflict, DuplicateEvent, GraphVersionConflict, OperationConflict


ROOT = Path(__file__).resolve().parents[1]


def migration_config(url):
    config = Config(str(ROOT / "alembic.ini"))
    config.attributes["database_url"] = url
    return config


@pytest.fixture(params=["sqlite", pytest.param("postgresql", marks=pytest.mark.postgres)])
def database(request, tmp_path):
    admin = None
    schema_name = None
    if request.param == "postgresql":
        base_url = os.environ.get("ANCHOR_TEST_POSTGRES_URL")
        if not base_url:
            pytest.skip("set ANCHOR_TEST_POSTGRES_URL to run the PostgreSQL contract")
        schema_name = f"anchor_test_{uuid4().hex}"
        admin = sa.create_engine(base_url)
        with admin.begin() as connection:
            connection.execute(sa.schema.CreateSchema(schema_name))
        url = make_url(base_url).update_query_dict({"options": f"-csearch_path={schema_name}"}).render_as_string(hide_password=False)
    else:
        url = f"sqlite:///{tmp_path / 'relational.sqlite'}"
    store = None
    try:
        command.upgrade(migration_config(url), "head")
        store = RelationalStateStore(url)
        yield store, url
    finally:
        if store:
            store.close()
        if admin:
            # Only the uniquely named schema created by this fixture is removed.
            with admin.begin() as connection:
                connection.execute(sa.schema.DropSchema(schema_name, cascade=True))
            admin.dispose()


def seed(store):
    version = store.publish_graph(GraphVersion.publish(GraphDefinition(
        graph_id="review", name="Review",
        nodes=[GraphNode(id="research", name="Research", type="agent", agent_ref="research-v1")],
    ), 1))
    trigger = store.create_trigger(Trigger(graph_version_id=version.graph_version_id, type="manual"))
    request = RunRequest(trigger_id=trigger.id, idempotency_key="occurrence-1", objective="Review evidence",
                         inputs={"options": {"verify": True}, "artifact": "report-1"})
    return version, request


def counts(store):
    with store.engine.connect() as connection:
        return {table.name: connection.scalar(sa.select(sa.func.count()).select_from(table))
                for table in (s.tasks, s.runs, s.node_runs, s.events, s.run_outbox, s.run_admissions)}


def test_admission_roundtrip_and_duplicate_contract(database):
    store, url = database
    version, request = seed(store)
    receipt = store.admit_run(request)
    reordered = request.model_copy(update={"inputs": {"artifact": "report-1", "options": {"verify": True}}})
    assert store.admit_run(reordered) == receipt
    assert counts(store) == dict(tasks=1, runs=1, node_runs=1, events=2, run_outbox=1, run_admissions=1)
    assert store.get_task(receipt.task_id).objective == request.objective
    assert store.get_run(receipt.run_id).graph_version_id == version.graph_version_id
    assert store.list_node_runs(receipt.run_id)[0].node_id == "research"
    assert store.pending_dispatches()[0].inputs == request.inputs
    reopened = RelationalStateStore(url)
    try:
        assert reopened.admit_run(request) == receipt
    finally:
        reopened.close()


def test_recovery_preserves_tool_evidence_and_rejects_old_lease(database):
    store, url = database
    _, request = seed(store)
    receipt = store.admit_run(request)
    store.accept_dispatch(store.pending_dispatches()[0])
    lease = store.claim_ready_agent_node("original", uuid4())
    operation = ToolOperation.register(operation_id=uuid4(), claim_id=lease.claim_id,
        run_id=lease.run_id, node_run_id=lease.node_run_id, tool_ref="scholarly.search", arguments={"query": "test"})
    store.register_tool_operation(operation)
    store.start_tool_operation(operation.operation_id, lease.claim_id)
    store.finish_tool_operation(operation.operation_id, lease.claim_id,
                                 status=OperationStatus.SUCCEEDED, result_ref="artifact://sha256/" + "a" * 64)
    # Upgrade with real dependent evidence, not just an empty schema.
    command.downgrade(migration_config(url), "0011_decision_attempts")
    command.upgrade(migration_config(url), "head")
    store.recover_node_lease(lease.claim_id, reason="worker confirmed stopped")
    replacement = store.claim_ready_agent_node("replacement", uuid4())
    assert replacement.node_run_id == lease.node_run_id
    assert replacement.claim_id != lease.claim_id
    with pytest.raises(ConcurrencyConflict, match="already released"):
        store.recover_node_lease(lease.claim_id, reason="stale recovery request")
    assert store.list_tool_operations(receipt.run_id)[0].claim_id == lease.claim_id
    assert [item.claim_id for item in store.list_active_leases()] == [replacement.claim_id]
    with store.engine.connect() as connection:
        assert connection.scalar(sa.select(sa.func.count()).select_from(s.node_leases)) == 2
        if store.engine.dialect.name == "sqlite":
            assert connection.exec_driver_sql("PRAGMA foreign_key_check").all() == []
    with pytest.raises(RuntimeError, match="lease history"):
        command.downgrade(migration_config(url), "0011_decision_attempts")
    store.check_schema()


def test_complete_node_propagates_and_completes_terminal_graph(database):
    store, _ = database
    version, request = seed(store)
    # Add a terminal verifier so the first node has a real downstream target.
    definition = version.definition.model_copy(deep=True)
    definition.nodes.append(GraphNode(id="verify", name="Verify", type="agent", agent_ref="verify-v1"))
    from anchor.domain.graph import GraphEdge
    definition.edges.append(GraphEdge(source="research", target="verify"))
    version = store.publish_graph(GraphVersion.publish(definition, 2))
    trigger = store.create_trigger(Trigger(graph_version_id=version.graph_version_id, type="manual"))
    request = request.model_copy(update={"trigger_id": trigger.id})
    receipt = store.admit_run(request)
    asyncio.run(DurableExecutionReceiver(store).accept(store.pending_dispatches()[0]))
    lease = store.claim_ready_node("worker", uuid4())
    assert lease is not None
    ready = store.complete_node_and_propagate(lease.claim_id, "worker", output_ref="artifact://sha256/a",
                                              node_input_hash="hash-a")
    assert [n.node_id for n in ready] == ["verify"]
    assert {node.node_id: node for node in store.list_node_runs(receipt.run_id)}["research"].input_hash == "hash-a"
    completed_events = [event for event in store.list_events(receipt.run_id)
                        if event["event_type"] == "node.completed"]
    assert completed_events[0]["payload"]["input_hash"] == "hash-a"
    lease = store.claim_ready_node("worker", uuid4())
    store.complete_node_and_propagate(lease.claim_id, "worker", output_ref="artifact://sha256/b")
    assert store.get_run(receipt.run_id).status is RunStatus.COMPLETED
    assert store.get_task(receipt.task_id).status.value == "completed"
    assert [event["event_type"] for event in store.list_events(receipt.task_id)][-1] == "task.completed"


def test_fail_node_marks_run_failed_and_is_idempotent(database):
    store, _ = database
    _, request = seed(store)
    receipt = store.admit_run(request)
    asyncio.run(DurableExecutionReceiver(store).accept(store.pending_dispatches()[0]))
    lease = store.claim_ready_node("worker-a", uuid4())
    failed = store.fail_node_and_propagate(lease.claim_id, "worker-a", error_code="model_unavailable", phase="model")
    assert failed.status.value == "failed"
    assert failed.error_code == "model_unavailable"
    assert store.get_run(receipt.run_id).status is RunStatus.FAILED
    assert store.get_task(receipt.task_id).status.value == "failed"
    assert store.fail_node_and_propagate(lease.claim_id, "worker-a", error_code="model_unavailable", phase="model") == failed
    with pytest.raises(ConcurrencyConflict, match="different error"):
        store.fail_node_and_propagate(lease.claim_id, "worker-a", error_code="other")
    assert [event["event_type"] for event in store.list_events(receipt.run_id)][-2:] == ["node.failed", "run.failed"]
    assert [event["event_type"] for event in store.list_events(receipt.task_id)][-1] == "task.failed"


def test_failed_run_never_revives_or_opens_downstream(database):
    store, _ = database
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
    _, request = seed(store)
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
    assert store.get_run(receipt.run_id).status is RunStatus.FAILED
    nodes = {node.node_id: node for node in store.list_node_runs(receipt.run_id)}
    assert nodes["left"].status.value == "failed"
    assert nodes["right"].status.value == "completed"
    assert nodes["join"].status.value == "pending"
    assert "run.completed" not in [event["event_type"] for event in store.list_events(receipt.run_id)]


def test_agent_claim_skips_ready_nodes_without_agent_executor(database):
    store, _ = database
    _, request = seed(store)
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


def test_control_claim_only_takes_control_nodes(database):
    store, _ = database
    _, request = seed(store)
    definition = GraphDefinition(
        graph_id="control-claim-filter", name="Control claim filter",
        nodes=[
            GraphNode(id="start", type="agent", name="Start", agent_ref="start-v1"),
            GraphNode(id="route", type="router", name="Route"),
            GraphNode(id="verify", type="verifier", name="Verify", verifier_ref="verify-v1"),
            GraphNode(id="next", type="agent", name="Next", agent_ref="next-v1"),
        ],
        edges=[
            GraphEdge(source="start", target="route"),
            GraphEdge(source="start", target="verify"),
            GraphEdge(source="start", target="next"),
        ],
    )
    version = store.publish_graph(GraphVersion.publish(definition, 1))
    trigger = store.create_trigger(Trigger(graph_version_id=version.graph_version_id, type="manual"))
    receipt = store.admit_run(request.model_copy(update={
        "trigger_id": trigger.id, "idempotency_key": "control-filter",
    }))
    asyncio.run(DurableExecutionReceiver(store).accept(store.pending_dispatches()[0]))
    start = store.claim_ready_agent_node("agent-worker", uuid4())
    store.complete_node_and_propagate(
        start.claim_id, "agent-worker", output_ref="artifact://start",
    )
    control = store.claim_ready_control_node("control-worker", uuid4())
    assert control is not None and control.node_id == "route"
    agent = store.claim_ready_agent_node("agent-worker", uuid4())
    assert agent is not None and agent.node_id == "next"
    remaining = {item.node_id: item.status.value for item in store.list_node_runs(receipt.run_id)}
    assert remaining["verify"] == "ready"


def test_verifier_claim_evidence_gate_and_recovery_contract(database):
    store, url = database
    definition = GraphDefinition(
        graph_id="verifier-contract",
        name="Verifier contract",
        nodes=[GraphNode(
            id="verify", type="verifier", name="Verify", verifier_ref="verifiers.contract",
        )],
    )
    version = store.publish_graph(GraphVersion.publish(definition, 1))
    trigger = store.create_trigger(Trigger(graph_version_id=version.graph_version_id, type="manual"))
    request = RunRequest(
        trigger_id=trigger.id,
        idempotency_key="verifier-contract",
        objective="Verify durable input",
        inputs={"approved": True},
    )
    receipt = store.admit_run(request)
    asyncio.run(DurableExecutionReceiver(store).accept(store.pending_dispatches()[0]))
    assert store.claim_ready_agent_node("agent-worker", uuid4()) is None
    assert store.claim_ready_control_node("control-worker", uuid4()) is None
    first = store.claim_ready_verifier_node("verifier-worker", uuid4())
    assert first is not None
    snapshot = {"inputs": {"approved": True}}
    evidence_ref = "artifact://sha256/" + "a" * 64
    with pytest.raises(ConcurrencyConflict, match="verification verdict"):
        store.complete_node_and_propagate(
            first.claim_id,
            "verifier-worker",
            output_ref=evidence_ref,
            input_snapshot=snapshot,
        )
    assert store.list_verifications(receipt.run_id) == []
    assert store.get_context_snapshot(first.node_run_id) is None

    recovered = store.recover_node_lease(
        first.claim_id, reason="operator confirmed verifier process exit",
    )
    assert recovered.status.value == "ready"
    lease = store.claim_ready_verifier_node("verifier-worker-2", uuid4())
    record = VerificationRecord(
        claim_id=lease.claim_id,
        run_id=lease.run_id,
        node_run_id=lease.node_run_id,
        node_id=lease.node_id,
        verifier_ref="verifiers.contract",
        verifier_version="v1",
        adapter="deterministic_test",
        adapter_version="v1",
        verdict="passed",
        reason="contract evidence passed",
        evidence_ref=evidence_ref,
        verified_context_hash=input_hash(snapshot),
    )
    store.complete_node_and_propagate(
        lease.claim_id,
        "verifier-worker-2",
        output_ref=evidence_ref,
        input_snapshot=snapshot,
        verification=record,
    )
    assert store.get_run(receipt.run_id).status is RunStatus.COMPLETED
    assert store.list_verifications(receipt.run_id) == [record]

    rejected_receipt = store.admit_run(request.model_copy(update={
        "idempotency_key": "verifier-contract-rejected",
    }))
    rejected_dispatch = next(
        item for item in store.pending_dispatches() if item.run_id == rejected_receipt.run_id
    )
    asyncio.run(DurableExecutionReceiver(store).accept(rejected_dispatch))
    rejected_lease = store.claim_ready_verifier_node("verifier-worker-3", uuid4())
    rejected_snapshot = {"inputs": {"approved": True}, "review": "insufficient"}
    rejected_record = VerificationRecord(
        claim_id=rejected_lease.claim_id,
        run_id=rejected_lease.run_id,
        node_run_id=rejected_lease.node_run_id,
        node_id=rejected_lease.node_id,
        verifier_ref="verifiers.contract",
        verifier_version="v1",
        adapter="deterministic_test",
        adapter_version="v1",
        verdict="rejected",
        reason="contract evidence rejected",
        evidence_ref="artifact://sha256/" + "b" * 64,
        verified_context_hash=input_hash(rejected_snapshot),
    )
    store.fail_node_and_propagate(
        rejected_lease.claim_id,
        "verifier-worker-3",
        error_code="verification_rejected",
        phase="verification",
        verification=rejected_record,
        input_snapshot=rejected_snapshot,
    )
    assert store.get_run(rejected_receipt.run_id).status is RunStatus.FAILED
    assert store.list_verifications(rejected_receipt.run_id) == [rejected_record]
    rejected_node = store.list_node_runs(rejected_receipt.run_id)[0]
    assert store.get_context_snapshot(rejected_node.id).input_hash == input_hash(rejected_snapshot)

    reopened = RelationalStateStore(url)
    try:
        assert reopened.list_verifications(receipt.run_id) == [record]
        assert reopened.list_verifications(rejected_receipt.run_id) == [rejected_record]
    finally:
        reopened.close()


def test_context_snapshot_is_durable_and_hash_verified(database):
    store, url = database
    version, request = seed(store)
    receipt = store.admit_run(request)
    asyncio.run(DurableExecutionReceiver(store).accept(store.pending_dispatches()[0]))
    lease = store.claim_ready_node("worker", uuid4())
    snapshot = {"inputs": {"topic": "durable"}, "sources": ["artifact://one"]}
    store.complete_node_and_propagate(lease.claim_id, "worker", output_ref="artifact://sha256/a",
                                      input_snapshot=snapshot)
    node = store.list_node_runs(receipt.run_id)[0]
    saved = store.get_context_snapshot(node.id)
    assert saved is not None and saved.snapshot == snapshot
    assert saved.input_hash == node.input_hash and saved.generation == 1
    completed_event = next(event for event in store.event_page(receipt.run_id)
                           if event["event_type"] == "node.completed")
    assert completed_event["payload"]["context_generation"] == 1
    assert store.get_run(receipt.run_id).context_generation == 1

    reopened = RelationalStateStore(url)
    try:
        assert reopened.list_context_snapshots(receipt.run_id) == [saved]
    finally:
        reopened.close()


def test_context_snapshot_hash_mismatch_is_atomic(database):
    store, _ = database
    _, request = seed(store)
    receipt = store.admit_run(request)
    asyncio.run(DurableExecutionReceiver(store).accept(store.pending_dispatches()[0]))
    lease = store.claim_ready_node("worker", uuid4())
    with pytest.raises(ValueError, match="does not match"):
        store.complete_node_and_propagate(lease.claim_id, "worker", output_ref="artifact://sha256/x",
                                          node_input_hash="0" * 64, input_snapshot={"value": 1})
    assert store.get_context_snapshot(lease.node_run_id) is None
    assert store.list_node_runs(receipt.run_id)[0].status.value == "running"


def test_conditional_branch_decisions_skip_rejected_path_and_join(database):
    store, _ = database
    _, request = seed(store)
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
    ready = store.complete_node_and_propagate(
        route.claim_id, "worker", output_ref="artifact://route",
        condition_context={"output": {"approved": True}, "inputs": {}},
    )
    assert [item.node_id for item in ready] == ["left"]
    nodes = {item.node_id: item for item in store.list_node_runs(receipt.run_id)}
    assert nodes["right"].status.value == "skipped"
    assert nodes["join"].status.value == "pending"

    left = store.claim_ready_node("worker", uuid4())
    ready = store.complete_node_and_propagate(
        left.claim_id, "worker", output_ref="artifact://left",
    )
    assert [item.node_id for item in ready] == ["join"]
    join = store.claim_ready_node("worker", uuid4())
    store.complete_node_and_propagate(join.claim_id, "worker", output_ref="artifact://join")

    decisions = store.list_edge_decisions(receipt.run_id)
    assert [(item.edge_index, item.selected, item.reason.value) for item in decisions] == [
        (0, True, "condition_true"),
        (1, False, "condition_false"),
        (2, True, "unconditional"),
        (3, False, "upstream_skipped"),
    ]
    assert store.get_run(receipt.run_id).status is RunStatus.COMPLETED
    events = store.list_events(receipt.run_id)
    assert store.get_run(receipt.run_id).last_event_sequence == events[-1]["sequence"]


def test_conflicting_key_and_disabled_trigger(database):
    store, _ = database
    version, request = seed(store)
    receipt = store.admit_run(request)
    with pytest.raises(AdmissionConflict):
        store.admit_run(request.model_copy(update={"objective": "different"}))
    with store.engine.begin() as connection:
        connection.execute(sa.update(s.triggers).values(enabled=False))
    assert store.admit_run(request) == receipt
    with pytest.raises(ValueError, match="disabled"):
        store.admit_run(request.model_copy(update={"idempotency_key": "new"}))
    assert counts(store)["runs"] == 1


def test_nested_write_failure_rolls_back(database, monkeypatch):
    store, _ = database
    _, request = seed(store)
    def fail(connection, message):
        raise RuntimeError("outbox unavailable")
    with monkeypatch.context() as patch:
        patch.setattr(store, "_enqueue_dispatch", fail)
        with pytest.raises(RuntimeError):
            store.admit_run(request)
    assert not any(counts(store).values())
    store.admit_run(request)
    assert counts(store)["runs"] == 1


@pytest.mark.parametrize("committed", [False, True])
def test_process_exit_recovery(database, committed):
    store, url = database
    _, request = seed(store)
    script = """
import os
from anchor.state.relational import RelationalStateStore
from anchor.domain.admission import RunRequest
store = RelationalStateStore(os.environ['ANCHOR_CHILD_DATABASE_URL'])
if os.environ['ANCHOR_CHILD_COMMIT'] == 'False':
    def crash(connection, message):
        os._exit(73)
    store._enqueue_dispatch = crash
store.admit_run(RunRequest.model_validate_json(os.environ['ANCHOR_CHILD_REQUEST']))
os._exit(73)
"""
    env = dict(os.environ, PYTHONPATH=str(ROOT / "src"), ANCHOR_CHILD_DATABASE_URL=url,
               ANCHOR_CHILD_COMMIT=str(committed), ANCHOR_CHILD_REQUEST=request.model_dump_json())
    result = subprocess.run([sys.executable, "-c", script], env=env, capture_output=True, text=True, timeout=20)
    assert result.returncode == 73, result.stderr
    assert counts(store)["runs"] == int(committed)
    receipt = store.admit_run(request)
    assert store.admit_run(request) == receipt
    assert counts(store)["runs"] == 1


def test_concurrent_duplicate_admission(database):
    store, url = database
    _, request = seed(store)
    other = RelationalStateStore(url)
    barrier = Barrier(2)
    def submit(adapter):
        barrier.wait(timeout=10)
        return adapter.admit_run(request)
    try:
        with ThreadPoolExecutor(max_workers=2) as pool:
            receipts = list(pool.map(submit, [store, other]))
        assert receipts[0] == receipts[1]
        assert counts(store)["runs"] == 1
    finally:
        other.close()


def test_concurrent_event_sequence_and_stale_revision(database):
    store, _ = database
    _, request = seed(store)
    receipt = store.admit_run(request)
    def append(index):
        return store.append_event(stream_id=receipt.run_id, event_type="observation",
                                   payload={"index": index}, idempotency_key=f"event-{index}")
    with ThreadPoolExecutor(max_workers=4) as pool:
        sequences = list(pool.map(append, range(8)))
    assert sorted(sequences) == list(range(2, 10))
    run = store.transition_run(run_id=receipt.run_id, expected_revision=0, status=RunStatus.RUNNING,
                              phase="work", payload={}, idempotency_key="start")
    assert run.revision == 1
    assert run.last_event_sequence == 10
    with pytest.raises(ConcurrencyConflict):
        store.transition_run(run_id=receipt.run_id, expected_revision=0, status=RunStatus.COMPLETED,
                             phase="done", payload={}, idempotency_key="stale")
    with pytest.raises(DuplicateEvent):
        store.append_event(stream_id=receipt.run_id, event_type="changed", payload={}, idempotency_key="start")
    assert len(store.list_events(receipt.run_id)) == 10


def test_pinned_graph_and_membership(database):
    store, _ = database
    version, request = seed(store)
    receipt = store.admit_run(request)
    version.definition.nodes[0].agent_ref = "new-model"
    with pytest.raises(GraphVersionConflict):
        store.publish_graph(version)
    store.publish_graph(GraphVersion.publish(version.definition, 2))
    assert store.get_run(receipt.run_id).graph_version_id == version.graph_version_id
    assert store.get_graph_version(version.graph_version_id).definition.nodes[0].agent_ref == "research-v1"
    with pytest.raises(ValueError, match="node does not belong"):
        store.create_node_run(NodeRun(run_id=receipt.run_id, node_id="foreign"))


def test_outbox_redelivery_and_acknowledgement(database, monkeypatch):
    store, _ = database
    _, request = seed(store)
    receipt = store.admit_run(request)
    accepted = set()
    class Target:
        async def accept(self, message):
            accepted.add(message.message_id)
    def fail(message_id):
        raise ConnectionError("acknowledgement lost")
    with monkeypatch.context() as patch:
        patch.setattr(store, "acknowledge_dispatch", fail)
        with pytest.raises(ConnectionError):
            asyncio.run(dispatch_pending(store, Target()))
    assert store.pending_dispatches()[0].message_id == receipt.message_id
    assert asyncio.run(dispatch_pending(store, Target())) == 1
    assert accepted == {receipt.message_id}
    assert store.pending_dispatches() == []


def test_receiver_accepts_dispatch_once_and_replay_is_safe(database):
    store, _ = database
    _, request = seed(store)
    receipt = store.admit_run(request)
    message = store.pending_dispatches()[0]
    receiver = DurableExecutionReceiver(store)
    asyncio.run(receiver.accept(message))
    first = store.get_run(receipt.run_id)
    assert first is not None and first.status is RunStatus.QUEUED and first.revision == 1
    assert [event["event_type"] for event in store.event_page(receipt.run_id)] == [
        "run.requested", "run.dispatch_accepted", "node.ready"
    ]
    assert store.list_node_runs(receipt.run_id)[0].status.value == "ready"
    assert store.accepted_dispatches() == [message]
    asyncio.run(receiver.accept(message))
    second = store.get_run(receipt.run_id)
    assert second is not None and second.revision == first.revision
    assert len(store.event_page(receipt.run_id)) == 3
    assert store.get_task(receipt.task_id).status.value == "ready"


def test_runtime_heartbeat_reports_recent_receiver_only(database):
    store, _ = database
    assert not store.runtime_connected("execution_receiver")
    store.record_runtime_heartbeat("execution_receiver", uuid4())
    assert store.runtime_connected("execution_receiver")
    with pytest.raises(ValueError, match="positive"):
        store.runtime_connected("execution_receiver", within_seconds=0)


def test_ready_node_claim_is_stable_and_owner_checked(database):
    store, _ = database
    _, request = seed(store)
    receipt = store.admit_run(request)
    asyncio.run(DurableExecutionReceiver(store).accept(store.pending_dispatches()[0]))
    claim_id = uuid4()
    lease = store.claim_ready_node("worker-a", claim_id)
    assert lease is not None and lease.node_id == "research"
    assert store.claim_ready_node("worker-a", claim_id) == lease
    assert store.claim_ready_node("worker-b", uuid4()) is None
    with pytest.raises(ConcurrencyConflict, match="another worker"):
        store.claim_ready_node("worker-b", claim_id)
    assert store.heartbeat_node_lease(claim_id, "worker-a").heartbeat_at >= lease.heartbeat_at
    with pytest.raises(ConcurrencyConflict, match="another worker"):
        store.heartbeat_node_lease(claim_id, "worker-b")
    run = store.get_run(receipt.run_id)
    assert run is not None and run.status is RunStatus.RUNNING and run.revision == 2
    assert store.list_node_runs(receipt.run_id)[0].status.value == "running"


def test_concurrent_workers_cannot_claim_the_same_ready_node(database):
    store, url = database
    _, request = seed(store)
    store.admit_run(request)
    asyncio.run(DurableExecutionReceiver(store).accept(store.pending_dispatches()[0]))
    other = RelationalStateStore(url)
    barrier = Barrier(2)
    def claim(adapter, worker):
        barrier.wait(timeout=10)
        return adapter.claim_ready_node(worker, uuid4())
    try:
        with ThreadPoolExecutor(max_workers=2) as pool:
            leases = list(pool.map(lambda args: claim(*args), [(store, "worker-a"), (other, "worker-b")]))
        assert sum(lease is not None for lease in leases) == 1
    finally:
        other.close()


def test_tool_operation_ledger_is_replay_safe(database):
    store, _ = database
    _, request = seed(store)
    store.admit_run(request)
    asyncio.run(DurableExecutionReceiver(store).accept(store.pending_dispatches()[0]))
    lease = store.claim_ready_node("worker-a", uuid4())
    assert lease is not None
    operation = ToolOperation.register(operation_id=uuid4(), claim_id=lease.claim_id,
        node_run_id=lease.node_run_id, run_id=lease.run_id, tool_ref="artifact.write",
        arguments={"path": "report.md", "content_hash": "abc"})
    assert store.register_tool_operation(operation) == operation
    assert store.register_tool_operation(operation) == operation
    running = store.start_tool_operation(operation.operation_id, lease.claim_id)
    assert running.status is OperationStatus.RUNNING
    completed = store.finish_tool_operation(operation.operation_id, lease.claim_id,
        status=OperationStatus.SUCCEEDED, result_ref="artifact://report/1")
    assert completed.status is OperationStatus.SUCCEEDED
    assert store.finish_tool_operation(operation.operation_id, lease.claim_id,
        status=OperationStatus.SUCCEEDED, result_ref="artifact://report/1") == completed
    with pytest.raises(OperationConflict, match="another lease"):
        store.start_tool_operation(operation.operation_id, uuid4())
    assert store.list_tool_operations(lease.run_id) == [completed]


def test_migration_upgrade_check_downgrade_roundtrip(database):
    store, url = database
    command.check(migration_config(url))
    store.close()
    command.downgrade(migration_config(url), "base")
    assert "runs" not in sa.inspect(store.engine).get_table_names()
    command.upgrade(migration_config(url), "head")
    assert "runs" in sa.inspect(store.engine).get_table_names()


def test_runtime_migrations_preserve_existing_run_and_outbox(database):
    store, url = database
    _, request = seed(store)
    receipt = store.admit_run(request)
    before = counts(store)
    # No drafts exist here. Only the new editor tables are removed, so this
    # reproduces upgrading an existing revision-0001 admission database.
    command.downgrade(migration_config(url), "0001")
    command.upgrade(migration_config(url), "head")
    assert counts(store) == before
    assert store.admit_run(request) == receipt
    assert store.pending_dispatches()[0].message_id == receipt.message_id
