"""Tool node execution through the control worker.

A tool node runs its tool under an explicit owner agent with arguments from
its resolved snapshot. Results persist as ledger-backed artifacts and open
downstream edges; denials and failures close the node loudly instead of
stalling it.
"""

import asyncio
import json
from uuid import uuid4

import pytest

from anchor.domain.admission import RunRequest
from anchor.domain.graph import GraphDefinition, GraphEdge, GraphNode, GraphVersion, Trigger
from anchor.runtime.artifacts import LocalArtifactStore
from anchor.runtime.capabilities import AgentCapability, CapabilityRegistry, ToolCapability
from anchor.runtime.control_worker import ControlNodeWorker
from anchor.runtime.dispatch import dispatch_pending
from anchor.runtime.receiver import DurableExecutionReceiver
from anchor.runtime.sinks import ArtifactCheckpointSink
from anchor.runtime.tool_gateway import SubprocessBackend, ToolGateway
from conftest import make_store


def registry():
    return CapabilityRegistry(
        models=[],
        agents=[AgentCapability(ref="agents.reader", model_ref="models.test",
                                tool_refs=["echo", "db.write"])],
        tools=[ToolCapability(ref="echo", description="emit args",
                              side_effect=False, operation_kind="read"),
               ToolCapability(ref="db.write", description="write rows",
                              side_effect=True, operation_kind="write")],
    )


def setup(tmp_path, tool_node, name="toolnode.sqlite", edges=None):
    store = make_store(tmp_path, name)
    artifacts = LocalArtifactStore(tmp_path / "artifacts")
    start = GraphNode(id="start", type="agent", name="Start", agent_ref="agents.reader")
    end = GraphNode(id="end", type="agent", name="End", agent_ref="agents.reader")
    nodes = [start, tool_node, end]
    if edges is None:
        edges = [GraphEdge(source="start", target="t",
                           input_mapping={"args": "outputs.start"}),
                 GraphEdge(source="t", target="end")]
    definition = GraphDefinition(graph_id="toolnode", name="ToolNode",
                                 nodes=nodes, edges=edges)
    store.publish_graph(GraphVersion.publish(definition, 1))
    version = store.list_graph_versions("toolnode")[0]
    trigger = store.create_trigger(Trigger(graph_version_id=version.graph_version_id,
                                           type="manual"))
    receipt = store.admit_run(RunRequest(trigger_id=trigger.id,
                                         idempotency_key=f"tool-{uuid4().hex}",
                                         objective="tool", inputs={}))
    asyncio.run(dispatch_pending(store, DurableExecutionReceiver(store)))
    gateway = ToolGateway(store, registry(), artifacts, SubprocessBackend())
    worker = ControlNodeWorker(store, artifacts,
                               ArtifactCheckpointSink(store, artifacts, "control"),
                               tools=gateway, registry=registry())
    return store, artifacts, receipt, worker


def tool_node(**overrides):
    spec = {"id": "t", "type": "tool", "name": "T", "tool_ref": "echo",
            "metadata": {"owner_agent": "agents.reader"}}
    spec.update(overrides)
    return GraphNode(**spec)


def complete_agent(store, artifacts, node_id, output):
    lease = store.claim_ready_node("worker", uuid4())
    assert lease is not None and lease.node_id == node_id
    ref = artifacts.put_text(output)
    store.complete_node_and_propagate(lease.claim_id, "worker", output_ref=ref,
                                      input_snapshot={"inputs": {}})
    return ref


def test_tool_node_echoes_through_gateway_to_terminal(tmp_path):
    store, artifacts, receipt, worker = setup(tmp_path, tool_node())
    try:
        complete_agent(store, artifacts, "start", json.dumps(["hi", "there"]))
        outcome = asyncio.run(worker.execute_once(worker_id="control"))
        assert outcome is not None and outcome.node_id == "t"
        assert artifacts.get_text(outcome.output_ref) == "hi there\n"
        nxt = store.claim_ready_node("worker", uuid4())
        assert nxt is not None and nxt.node_id == "end"
        ref = artifacts.put_text("done")
        store.complete_node_and_propagate(nxt.claim_id, "worker", output_ref=ref,
                                          input_snapshot={"inputs": {}})
        assert store.get_run(receipt.run_id).status.value == "completed"
    finally:
        store.close()


def test_tool_node_without_owner_fails_closed(tmp_path):
    node = tool_node(metadata={})
    store, artifacts, receipt, worker = setup(tmp_path, node, name="noowner.sqlite")
    try:
        complete_agent(store, artifacts, "start", json.dumps(["hi"]))
        with pytest.raises(ValueError, match="no owner_agent"):
            asyncio.run(worker.execute_once(worker_id="control"))
        failed = next(item for item in store.list_node_runs(receipt.run_id)
                      if item.node_id == "t")
        assert failed.status.value == "failed" and failed.error_code == "tool_no_owner"
    finally:
        store.close()


def test_tool_node_with_invalid_arguments_fails_closed(tmp_path):
    store, artifacts, receipt, worker = setup(tmp_path, tool_node(), name="badargs.sqlite")
    try:
        complete_agent(store, artifacts, "start", json.dumps("justastring"))
        with pytest.raises(ValueError, match="denied"):
            asyncio.run(worker.execute_once(worker_id="control"))
        failed = next(item for item in store.list_node_runs(receipt.run_id)
                      if item.node_id == "t")
        assert failed.status.value == "failed" and failed.error_code == "tool_denied"
    finally:
        store.close()


def test_side_effect_tool_fails_pending_tool_level_approval(tmp_path):
    node = tool_node(tool_ref="db.write")
    store, artifacts, receipt, worker = setup(tmp_path, node, name="sideeffect.sqlite")
    try:
        complete_agent(store, artifacts, "start", json.dumps(["hi"]))
        with pytest.raises(ValueError, match="denied"):
            asyncio.run(worker.execute_once(worker_id="control"))
        failed = next(item for item in store.list_node_runs(receipt.run_id)
                      if item.node_id == "t")
        assert failed.status.value == "failed"
    finally:
        store.close()


def test_tool_lease_is_never_recoverable_through_lease_path(tmp_path):
    import pytest as _pytest
    from anchor.runtime.supervisor import assess_leases
    from datetime import datetime, timedelta, timezone
    store, artifacts, receipt, worker = setup(tmp_path, tool_node(), name="norecover.sqlite")
    try:
        complete_agent(store, artifacts, "start", json.dumps(["hi"]))
        lease = store.claim_ready_control_node("control", uuid4())
        assert lease is not None and lease.node_id == "t"
        with _pytest.raises(Exception, match="can be recovered"):
            store.recover_node_lease(lease.claim_id, reason="operator")
        old = lease.model_copy(update={"heartbeat_at": datetime.now(timezone.utc) - timedelta(seconds=60)})
        assessment = assess_leases([old], node_types={str(old.claim_id): "tool"})[0]
        assert (assessment.state, assessment.recoverable) == ("unknown", False)
    finally:
        store.close()
