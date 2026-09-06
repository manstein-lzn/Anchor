"""Approval and event-wait durable states.

An approval/human gate parks in `waiting_approval` instead of executing;
`wait_for_event` parks in `waiting_event`. No worker claims waiting nodes.
Humans (or event ingress) advance them through atomic decide/resume calls
that share the lease path's propagation tail.
"""

import asyncio
from uuid import uuid4

import pytest

from anchor.domain.admission import RunRequest
from anchor.domain.graph import GraphDefinition, GraphEdge, GraphNode, GraphVersion, Trigger
from anchor.runtime.artifacts import LocalArtifactStore
from anchor.runtime.dispatch import dispatch_pending
from anchor.runtime.receiver import DurableExecutionReceiver
from conftest import make_store


def setup(tmp_path, nodes, edges, name="approval.sqlite", entry=None):
    store = make_store(tmp_path, name)
    definition = GraphDefinition(graph_id="gate", name="Gate", nodes=nodes, edges=edges,
                                 entry_node_id=entry)
    store.publish_graph(GraphVersion.publish(definition, 1))
    version = store.list_graph_versions("gate")[0]
    trigger = store.create_trigger(Trigger(graph_version_id=version.graph_version_id,
                                           type="manual"))
    receipt = store.admit_run(RunRequest(trigger_id=trigger.id,
                                         idempotency_key=f"gate-{uuid4().hex}",
                                         objective="gate", inputs={}))
    asyncio.run(dispatch_pending(store, DurableExecutionReceiver(store)))
    return store, receipt


def agent(node_id, ref="agents.a"):
    return GraphNode(id=node_id, type="agent", name=node_id, agent_ref=ref)


def test_approval_parks_and_blocks_terminal(tmp_path):
    nodes = [agent("a"), GraphNode(id="g", type="approval", name="G"), agent("b")]
    edges = [GraphEdge(source="a", target="g"), GraphEdge(source="g", target="b")]
    store, receipt = setup(tmp_path, nodes, edges)
    try:
        lease = store.claim_ready_node("worker", uuid4())
        assert lease is not None and lease.node_id == "a"
        artifacts = LocalArtifactStore(tmp_path / "artifacts")
        store.complete_node_and_propagate(lease.claim_id, "worker",
                                          output_ref=artifacts.put_text("a-out"),
                                          input_snapshot={"inputs": {}})
        waiting = {node.node_id: node for node in store.list_node_runs(receipt.run_id)}
        assert waiting["g"].status.value == "waiting_approval"
        assert waiting["b"].status.value == "pending"
        assert store.claim_ready_node("worker", uuid4()) is None
        assert store.get_run(receipt.run_id).status.value == "running"
        events = [event["event_type"] for event in store.list_events(receipt.run_id)]
        assert "node.waiting" in events
    finally:
        store.close()


def test_approve_opens_downstream_to_terminal(tmp_path):
    nodes = [agent("a"), GraphNode(id="g", type="approval", name="G"), agent("b")]
    edges = [GraphEdge(source="a", target="g"), GraphEdge(source="g", target="b")]
    store, receipt = setup(tmp_path, nodes, edges)
    try:
        artifacts = LocalArtifactStore(tmp_path / "artifacts")
        lease = store.claim_ready_node("worker", uuid4())
        store.complete_node_and_propagate(lease.claim_id, "worker",
                                          output_ref=artifacts.put_text("a-out"),
                                          input_snapshot={"inputs": {}})
        gate = next(node for node in store.list_node_runs(receipt.run_id)
                    if node.node_id == "g")
        decided = store.decide_approval(gate.id, approved=True, reason="looks right",
                                        actor="owner",
                                        output_ref=artifacts.put_text('{"ok":true}'))
        assert decided.status.value == "completed"
        nxt = store.claim_ready_node("worker", uuid4())
        assert nxt is not None and nxt.node_id == "b"
        store.complete_node_and_propagate(nxt.claim_id, "worker",
                                          output_ref=artifacts.put_text("b-out"),
                                          input_snapshot={"inputs": {}})
        assert store.get_run(receipt.run_id).status.value == "completed"
        assert store.get_task(receipt.task_id).status.value == "completed"
        events = [event["event_type"] for event in store.list_events(receipt.run_id)]
        assert "node.approved" in events
    finally:
        store.close()


def test_reject_fails_closed_without_downstream(tmp_path):
    nodes = [agent("a"), GraphNode(id="g", type="approval", name="G"), agent("b")]
    edges = [GraphEdge(source="a", target="g"), GraphEdge(source="g", target="b")]
    store, receipt = setup(tmp_path, nodes, edges)
    try:
        artifacts = LocalArtifactStore(tmp_path / "artifacts")
        lease = store.claim_ready_node("worker", uuid4())
        store.complete_node_and_propagate(lease.claim_id, "worker",
                                          output_ref=artifacts.put_text("a-out"),
                                          input_snapshot={"inputs": {}})
        gate = next(node for node in store.list_node_runs(receipt.run_id)
                    if node.node_id == "g")
        decided = store.decide_approval(gate.id, approved=False, reason="wrong branch",
                                        actor="owner", output_ref="artifact://unused")
        assert decided.status.value == "failed"
        assert decided.error_code == "approval_rejected"
        assert store.get_run(receipt.run_id).status.value == "failed"
        assert store.get_task(receipt.task_id).status.value == "failed"
        assert store.claim_ready_node("worker", uuid4()) is None
    finally:
        store.close()


def test_approval_decisions_are_replay_safe_and_conflict_checked(tmp_path):
    nodes = [GraphNode(id="g", type="approval", name="G"), agent("b")]
    edges = [GraphEdge(source="g", target="b")]
    store, receipt = setup(tmp_path, nodes, edges, entry="g")
    try:
        artifacts = LocalArtifactStore(tmp_path / "artifacts")
        gate = next(node for node in store.list_node_runs(receipt.run_id)
                    if node.node_id == "g")
        assert gate.status.value == "waiting_approval"
        ref = artifacts.put_text('{"ok":true}')
        first = store.decide_approval(gate.id, approved=True, reason="yes",
                                      actor="owner", output_ref=ref)
        again = store.decide_approval(gate.id, approved=True, reason="yes",
                                      actor="owner", output_ref=ref)
        assert again == first
        with pytest.raises(Exception, match="different output"):
            store.decide_approval(gate.id, approved=True, reason="yes",
                                  actor="owner", output_ref=artifacts.put_text('{"ok":false}'))
        with pytest.raises(Exception, match="not awaiting approval"):
            store.decide_approval(gate.id, approved=False, reason="no",
                                  actor="owner", output_ref=ref)
    finally:
        store.close()


def test_event_wait_parks_and_resumes_on_matching_type(tmp_path):
    nodes = [agent("a"),
             GraphNode(id="w", type="wait_for_event", name="W",
                       metadata={"wait_event": "build.finished"}),
             agent("b")]
    edges = [GraphEdge(source="a", target="w"), GraphEdge(source="w", target="b")]
    store, receipt = setup(tmp_path, nodes, edges)
    try:
        artifacts = LocalArtifactStore(tmp_path / "artifacts")
        lease = store.claim_ready_node("worker", uuid4())
        store.complete_node_and_propagate(lease.claim_id, "worker",
                                          output_ref=artifacts.put_text("a-out"),
                                          input_snapshot={"inputs": {}})
        waiter = next(node for node in store.list_node_runs(receipt.run_id)
                      if node.node_id == "w")
        assert waiter.status.value == "waiting_event"
        with pytest.raises(ValueError, match="does not match"):
            store.resume_event(waiter.id, event_type="other.event", payload={},
                               output_ref=artifacts.put_text("{}"))
        resumed = store.resume_event(waiter.id, event_type="build.finished",
                                     payload={"build": 7},
                                     output_ref=artifacts.put_text('{"build":7}'))
        assert resumed.status.value == "completed"
        nxt = store.claim_ready_node("worker", uuid4())
        assert nxt is not None and nxt.node_id == "b"
        store.complete_node_and_propagate(nxt.claim_id, "worker",
                                          output_ref=artifacts.put_text("b-out"),
                                          input_snapshot={"inputs": {}})
        assert store.get_run(receipt.run_id).status.value == "completed"
    finally:
        store.close()


def test_waiting_nodes_are_visible_for_operators(tmp_path):
    nodes = [GraphNode(id="g", type="approval", name="G")]
    store, receipt = setup(tmp_path, nodes, [], entry="g")
    try:
        waiting = store.list_waiting_nodes()
        assert [node.node_id for node in waiting] == ["g"]
        assert store.list_waiting_nodes(run_id=receipt.run_id)[0].run_id == receipt.run_id
    finally:
        store.close()
