import asyncio
from uuid import uuid4

from anchor.domain.admission import RunRequest
from anchor.domain.context import canonical_json
from anchor.domain.graph import GraphDefinition, GraphEdge, GraphNode, GraphVersion, Trigger
from anchor.runtime.artifacts import LocalArtifactStore
from anchor.runtime.control_worker import ControlNodeWorker
from anchor.runtime.control_service import run_control_loop
from anchor.runtime.dispatch import dispatch_pending
from anchor.runtime.receiver import DurableExecutionReceiver
from anchor.runtime.sinks import ArtifactCheckpointSink
from conftest import make_store


def make_worker(store, tmp_path):
    artifacts = LocalArtifactStore(tmp_path / "artifacts")
    worker_id = "control-worker"
    return (
        ControlNodeWorker(
            store, artifacts, ArtifactCheckpointSink(store, artifacts, worker_id),
        ),
        artifacts,
        worker_id,
    )


def admit(store, definition, key, inputs=None):
    version = store.publish_graph(GraphVersion.publish(definition, 1))
    trigger = store.create_trigger(Trigger(
        graph_version_id=version.graph_version_id, type="manual",
    ))
    receipt = store.admit_run(RunRequest(
        trigger_id=trigger.id,
        idempotency_key=key,
        objective=key,
        inputs=inputs or {},
    ))
    asyncio.run(dispatch_pending(store, DurableExecutionReceiver(store)))
    return receipt


def test_router_uses_canonical_input_and_only_control_worker_claims_it(tmp_path):
    store = make_store(tmp_path, "router.sqlite")
    try:
        definition = GraphDefinition(
            graph_id="router-control", name="Router control",
            nodes=[
                GraphNode(id="route", type="router", name="Route"),
                GraphNode(id="accepted", type="agent", name="Accepted", agent_ref="accepted"),
                GraphNode(id="rejected", type="agent", name="Rejected", agent_ref="rejected"),
            ],
            edges=[
                GraphEdge(source="route", target="accepted", condition="output.inputs.approved"),
                GraphEdge(source="route", target="rejected",
                          condition="output.inputs.approved == `false`"),
            ],
        )
        receipt = admit(store, definition, "router-control", {"approved": True})
        assert store.claim_ready_agent_node("agent-worker", uuid4()) is None
        worker, artifacts, worker_id = make_worker(store, tmp_path)
        outcome = asyncio.run(worker.execute_once(worker_id=worker_id))
        assert outcome is not None and outcome.node_id == "route"
        assert artifacts.get_text(outcome.output_ref) == canonical_json({
            "inputs": {"approved": True},
        })

        nodes = {item.node_id: item for item in store.list_node_runs(receipt.run_id)}
        assert nodes["route"].status.value == "completed"
        assert nodes["accepted"].status.value == "ready"
        assert nodes["rejected"].status.value == "skipped"
        assert store.claim_ready_control_node(worker_id, uuid4()) is None
        agent = store.claim_ready_agent_node("agent-worker", uuid4())
        assert agent is not None and agent.node_id == "accepted"
    finally:
        store.close()


def test_parallel_join_and_artifact_execute_without_model_results(tmp_path):
    store = make_store(tmp_path, "control-chain.sqlite")
    try:
        definition = GraphDefinition(
            graph_id="control-chain", name="Control chain",
            nodes=[
                GraphNode(id="parallel", type="parallel", name="Parallel"),
                GraphNode(id="left", type="agent", name="Left", agent_ref="left"),
                GraphNode(id="right", type="agent", name="Right", agent_ref="right"),
                GraphNode(id="join", type="join", name="Join"),
                GraphNode(id="artifact", type="artifact", name="Artifact"),
            ],
            edges=[
                GraphEdge(source="parallel", target="left"),
                GraphEdge(source="parallel", target="right"),
                GraphEdge(source="left", target="join", input_mapping={"left": "outputs.left"}),
                GraphEdge(source="right", target="join", input_mapping={"right": "outputs.right"}),
                GraphEdge(source="join", target="artifact"),
            ],
        )
        receipt = admit(store, definition, "control-chain")
        worker, artifacts, worker_id = make_worker(store, tmp_path)
        first = asyncio.run(worker.execute_once(worker_id=worker_id))
        assert first is not None and first.node_id == "parallel"

        left = store.claim_ready_agent_node("agent-left", uuid4())
        right = store.claim_ready_agent_node("agent-right", uuid4())
        assert {left.node_id, right.node_id} == {"left", "right"}
        by_node = {
            left.node_id: (left, "agent-left"),
            right.node_id: (right, "agent-right"),
        }
        store.complete_node_and_propagate(
            by_node["left"][0].claim_id, by_node["left"][1],
            output_ref=artifacts.put_text("LEFT"),
        )
        store.complete_node_and_propagate(
            by_node["right"][0].claim_id, by_node["right"][1],
            output_ref=artifacts.put_text("RIGHT"),
        )

        joined = asyncio.run(worker.execute_once(worker_id=worker_id))
        assert joined is not None and joined.node_id == "join"
        assert artifacts.get_text(joined.output_ref) == canonical_json({
            "left": "LEFT", "right": "RIGHT",
        })
        final = asyncio.run(worker.execute_once(worker_id=worker_id))
        assert final is not None and final.node_id == "artifact"
        assert store.get_run(receipt.run_id).status.value == "completed"
        assert store.get_task(receipt.task_id).status.value == "completed"
        assert all(
            item.status.value == "completed" for item in store.list_node_runs(receipt.run_id)
        )
    finally:
        store.close()


def test_interrupted_control_lease_requires_explicit_recovery(tmp_path):
    store = make_store(tmp_path, "control-recovery.sqlite")
    try:
        definition = GraphDefinition(
            graph_id="control-recovery", name="Control recovery",
            nodes=[GraphNode(id="artifact", type="artifact", name="Artifact")],
        )
        receipt = admit(store, definition, "control-recovery")
        lease = store.claim_ready_control_node("control-worker", uuid4())
        assert lease is not None
        assert store.claim_ready_control_node("other-worker", uuid4()) is None
        recovered = store.recover_node_lease(
            lease.claim_id, reason="operator confirmed control worker interruption",
        )
        assert recovered.status.value == "ready"
        reclaimed = store.claim_ready_control_node("other-worker", uuid4())
        assert reclaimed is not None and reclaimed.node_id == "artifact"
        assert store.get_run(receipt.run_id).status.value == "running"
    finally:
        store.close()


def test_control_loop_reports_heartbeat_and_stops_cleanly():
    stop = asyncio.Event()

    class Store:
        def __init__(self):
            self.heartbeats = 0

        def record_runtime_heartbeat(self, component, instance_id):
            assert component == "control_worker"
            self.heartbeats += 1

    class Worker:
        def __init__(self):
            self.store = Store()

        async def execute_once(self, *, worker_id):
            stop.set()
            return object()

    worker = Worker()
    asyncio.run(run_control_loop(
        worker, worker_id="control-worker", interval=0.001, stop=stop,
    ))
    assert worker.store.heartbeats == 1
