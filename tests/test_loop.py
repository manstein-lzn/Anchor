"""Loop execution: validated cycles iterate with per-attempt evidence.

A loop node is a deterministic conditional branch point that the validator
permits on cycles. The control worker executes it as pass-through; outgoing
conditions evaluate against its own resolved snapshot, so cycle edges carry
input mappings that expose body state. Each iteration re-decides its
outgoing edges under its source attempt — decisions never cross attempts.
No numeric iteration budget exists; repeated cycles are watchdog-visible,
never silently terminated.
"""

import asyncio
import json
from uuid import uuid4

import pytest

from anchor.domain.admission import RunRequest
from anchor.domain.conditions import build_condition_context
from anchor.domain.graph import GraphDefinition, GraphEdge, GraphNode, GraphVersion, Trigger
from anchor.runtime.artifacts import LocalArtifactStore
from anchor.runtime.control_worker import ControlNodeWorker
from anchor.runtime.dispatch import dispatch_pending
from anchor.runtime.receiver import DurableExecutionReceiver
from anchor.runtime.sinks import ArtifactCheckpointSink
from conftest import make_store


def setup(tmp_path, name="loop.sqlite"):
    store = make_store(tmp_path, name)
    artifacts = LocalArtifactStore(tmp_path / "artifacts")
    definition = GraphDefinition(
        graph_id="loop", name="Loop",
        nodes=[GraphNode(id="start", type="agent", name="Start", agent_ref="s"),
               GraphNode(id="gate", type="loop", name="Gate",
                         exit_condition="output.state.done == `true`"),
               GraphNode(id="work", type="agent", name="Work", agent_ref="w"),
               GraphNode(id="done", type="agent", name="Done", agent_ref="d")],
        edges=[GraphEdge(source="start", target="gate",
                         input_mapping={"state": "outputs.start"}),
               GraphEdge(source="gate", target="work",
                         condition="output.state.done == `false`"),
               GraphEdge(source="gate", target="done",
                         condition="output.state.done == `true`"),
               GraphEdge(source="work", target="gate",
                         input_mapping={"state": "outputs.work"})],
    )
    store.publish_graph(GraphVersion.publish(definition, 1))
    version = store.list_graph_versions("loop")[0]
    trigger = store.create_trigger(Trigger(graph_version_id=version.graph_version_id,
                                           type="manual"))
    receipt = store.admit_run(RunRequest(trigger_id=trigger.id,
                                         idempotency_key=f"loop-{uuid4().hex}",
                                         objective="loop", inputs={}))
    asyncio.run(dispatch_pending(store, DurableExecutionReceiver(store)))
    return store, artifacts, receipt


def complete_agent(store, artifacts, node_id, state):
    lease = store.claim_ready_node("worker", uuid4())
    assert lease is not None and lease.node_id == node_id, (
        f"expected {node_id}, got {lease}")
    ref = artifacts.put_text(json.dumps(state))
    store.complete_node_and_propagate(lease.claim_id, "worker", output_ref=ref,
                                      input_snapshot={"inputs": {}})
    return ref


def run_control_once(store, artifacts):
    worker = ControlNodeWorker(store, artifacts,
                               ArtifactCheckpointSink(store, artifacts, "control"))
    return asyncio.run(worker.execute_once(worker_id="control"))


def test_loop_iterates_then_exits_with_attempt_scoped_decisions(tmp_path):
    store, artifacts, receipt = setup(tmp_path)
    try:
        complete_agent(store, artifacts, "start", {"done": False})
        assert run_control_once(store, artifacts).node_id == "gate"
        complete_agent(store, artifacts, "work", {"done": False})
        assert run_control_once(store, artifacts).node_id == "gate"
        complete_agent(store, artifacts, "work", {"done": True})
        assert run_control_once(store, artifacts).node_id == "gate"
        # done was skipped in earlier iterations; the exit decision revives it.
        revived = next(node for node in store.list_node_runs(receipt.run_id)
                       if node.node_id == "done")
        assert revived.status.value == "ready"
        complete_agent(store, artifacts, "done", {"done": True})
        assert store.get_run(receipt.run_id).status.value == "completed"
        attempts = sorted((node.node_id, node.attempt)
                          for node in store.list_node_runs(receipt.run_id))
        assert attempts == [("done", 0), ("gate", 0), ("gate", 1), ("gate", 2),
                            ("start", 0), ("work", 0), ("work", 1)]
        decisions = {(d.edge_index, d.source_attempt): d.selected
                     for d in store.list_edge_decisions(receipt.run_id)}
        gate_work = next(i for i, e in
                         enumerate(store.get_graph_version(
                             store.get_run(receipt.run_id).graph_version_id
                         ).definition.edges)
                         if e.source == "gate" and e.target == "work")
        assert decisions[(gate_work, 0)] is True
        assert decisions[(gate_work, 2)] is False
    finally:
        store.close()


def test_loop_state_flows_through_mapped_snapshots(tmp_path):
    from anchor.runtime.resolution import resolve_node_context
    store, artifacts, receipt = setup(tmp_path, name="loop-state.sqlite")
    try:
        complete_agent(store, artifacts, "start", {"done": False, "n": 0})
        run_control_once(store, artifacts)
        resolved = resolve_node_context(store, receipt.run_id, "work", artifacts)
        assert resolved.snapshot == {"inputs": {}}
        complete_agent(store, artifacts, "work", {"done": True, "n": 1})
        run_control_once(store, artifacts)
        gate = resolve_node_context(store, receipt.run_id, "gate", artifacts)
        assert gate.snapshot == {"state": {"done": True, "n": 1}}
    finally:
        store.close()
