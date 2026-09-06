import asyncio
from uuid import uuid4
import pytest
from anchor.runtime.memory import LocalMemoryStore
from anchor.runtime.worker_service import _resolver
from anchor.runtime.artifacts import LocalArtifactStore
from anchor.runtime.model_gateway import ModelResponse
from anchor.runtime.sinks import ArtifactCheckpointSink
from anchor.domain.admission import RunRequest
from anchor.domain.graph import GraphDefinition, GraphEdge, GraphNode, GraphVersion, Trigger
from anchor.runtime.dispatch import dispatch_pending
from anchor.runtime.receiver import DurableExecutionReceiver
from conftest import make_store

def test_memory_store_is_readable_for_context(tmp_path):
    # The resolver needs a full state fixture; this test verifies the memory
    # projection independently until the service integration fixture is added.
    store = LocalMemoryStore(tmp_path / "memory.jsonl")
    run_id = uuid4()
    store.put(__import__('anchor.runtime.memory', fromlist=['MemoryRecord']).MemoryRecord.create("verified fact", run_id=run_id))
    assert [item.content for item in store.list(run_id=run_id)] == ["verified fact"]


def seed_chain(tmp_path, mapping):
    store = make_store(tmp_path, "chain.sqlite")
    definition = GraphDefinition(
        graph_id="chain", name="Chain",
        nodes=[GraphNode(id="a", type="agent", name="A", agent_ref="agents.a"),
               GraphNode(id="b", type="agent", name="B", agent_ref="agents.b")],
        edges=[GraphEdge(source="a", target="b", input_mapping=mapping)],
    )
    version = store.publish_graph(GraphVersion.publish(definition, 1))
    trigger = store.create_trigger(Trigger(graph_version_id=version.graph_version_id, type="manual"))
    receipt = store.admit_run(RunRequest(trigger_id=trigger.id, idempotency_key="chain-1",
                                         objective="chain", inputs={"topic": "lens"}))
    asyncio.run(dispatch_pending(store, DurableExecutionReceiver(store)))
    return store, receipt


def test_resolver_merges_predecessor_artifact_content(tmp_path):
    store, receipt = seed_chain(tmp_path, {"prior": "outputs.a"})
    try:
        artifacts = LocalArtifactStore(tmp_path / "artifacts")
        ref = artifacts.put_text("ALPHA")
        lease = store.claim_ready_node("worker", uuid4())
        assert lease is not None and lease.node_id == "a"
        store.complete_node_and_propagate(lease.claim_id, "worker", output_ref=ref)
        agent_ref, prompt, node_id, snapshot = asyncio.run(
            _resolver(store, receipt.run_id, "b", artifacts=artifacts))
        assert (agent_ref, node_id) == ("agents.b", "b")
        assert snapshot == {"prior": "ALPHA"}
        assert "ALPHA" in prompt
    finally:
        store.close()


def test_resolver_truncates_large_predecessor_text(tmp_path):
    store, receipt = seed_chain(tmp_path, {"prior": "outputs.a"})
    try:
        artifacts = LocalArtifactStore(tmp_path / "artifacts")
        ref = artifacts.put_text("x" * 5000)
        lease = store.claim_ready_node("worker", uuid4())
        store.complete_node_and_propagate(lease.claim_id, "worker", output_ref=ref)
        _, _, _, snapshot = asyncio.run(_resolver(store, receipt.run_id, "b", artifacts=artifacts))
        assert snapshot["prior"].endswith("[truncated:1000-chars]")
        assert len(snapshot["prior"]) == 4000 + len("\n[truncated:1000-chars]")
    finally:
        store.close()


def test_resolver_without_artifacts_keeps_reference(tmp_path):
    store, receipt = seed_chain(tmp_path, {"prior": "outputs.a"})
    try:
        lease = store.claim_ready_node("worker", uuid4())
        store.complete_node_and_propagate(lease.claim_id, "worker",
                                          output_ref="artifact://sha256/" + "a" * 64)
        _, _, _, snapshot = asyncio.run(_resolver(store, receipt.run_id, "b"))
        assert snapshot == {"prior": "artifact://sha256/" + "a" * 64}
    finally:
        store.close()


def test_resolver_reuses_canonical_snapshot_on_replay(tmp_path):
    store, receipt = seed_chain(tmp_path, {"prior": "outputs.a"})
    try:
        artifacts = LocalArtifactStore(tmp_path / "artifacts")
        lease = store.claim_ready_node("worker", uuid4())
        store.complete_node_and_propagate(
            lease.claim_id, "worker", output_ref=artifacts.put_text("ORIGINAL"),
            input_snapshot={"inputs": {"fixed": "canonical"}},
        )
        # A replay must use the persisted generation even if a mutable artifact
        # projection now contains a different value.
        artifacts.put_text("MUTATED")
        _, prompt, node_id, snapshot = asyncio.run(_resolver(
            store, receipt.run_id, "a", artifacts=artifacts))
        assert node_id == "a"
        assert snapshot == {"inputs": {"fixed": "canonical"}}
        assert "canonical" in prompt and "MUTATED" not in prompt
    finally:
        store.close()


def test_result_sink_routes_json_and_resolver_uses_selected_conditional_mapping(tmp_path):
    store = make_store(tmp_path, "conditional.sqlite")
    try:
        definition = GraphDefinition(
            graph_id="conditional-context", name="Conditional context",
            nodes=[
                GraphNode(id="route", type="agent", name="Route", agent_ref="agents.route"),
                GraphNode(id="accepted", type="agent", name="Accepted", agent_ref="agents.accepted"),
                GraphNode(id="rejected", type="agent", name="Rejected", agent_ref="agents.rejected"),
            ],
            edges=[
                GraphEdge(source="route", target="accepted", condition="output.approved",
                          input_mapping={"decision": "outputs.route"}),
                GraphEdge(source="route", target="rejected", condition="output.approved == `false`"),
            ],
        )
        version = store.publish_graph(GraphVersion.publish(definition, 1))
        trigger = store.create_trigger(Trigger(graph_version_id=version.graph_version_id, type="manual"))
        receipt = store.admit_run(RunRequest(
            trigger_id=trigger.id, idempotency_key="conditional-context",
            objective="route", inputs={"topic": "lens"},
        ))
        asyncio.run(dispatch_pending(store, DurableExecutionReceiver(store)))
        lease = store.claim_ready_node("worker", uuid4())
        artifacts = LocalArtifactStore(tmp_path / "artifacts")
        sink = ArtifactCheckpointSink(store, artifacts, "worker")
        response = ModelResponse(
            text='{"approved":true,"summary":"ALPHA"}', provider="test", model="test",
        )
        asyncio.run(sink.persist_model_result(
            claim_id=lease.claim_id, node_run_id=lease.node_run_id,
            response=response, input_snapshot={"inputs": {"topic": "lens"}},
        ))

        nodes = {item.node_id: item for item in store.list_node_runs(receipt.run_id)}
        assert nodes["accepted"].status.value == "ready"
        assert nodes["rejected"].status.value == "skipped"
        _, _, _, snapshot = asyncio.run(_resolver(
            store, receipt.run_id, "accepted", artifacts=artifacts,
        ))
        assert snapshot == {"decision": {"approved": True, "summary": "ALPHA"}}
    finally:
        store.close()


def test_result_sink_marks_invalid_condition_result_as_known_failure(tmp_path):
    store = make_store(tmp_path, "invalid-condition.sqlite")
    try:
        definition = GraphDefinition(
            graph_id="invalid-condition-result", name="Invalid condition result",
            nodes=[
                GraphNode(id="route", type="agent", name="Route", agent_ref="agents.route"),
                GraphNode(id="next", type="agent", name="Next", agent_ref="agents.next"),
            ],
            edges=[GraphEdge(source="route", target="next", condition="output.decision")],
        )
        version = store.publish_graph(GraphVersion.publish(definition, 1))
        trigger = store.create_trigger(Trigger(graph_version_id=version.graph_version_id, type="manual"))
        receipt = store.admit_run(RunRequest(
            trigger_id=trigger.id, idempotency_key="invalid-condition-result", objective="route",
        ))
        asyncio.run(dispatch_pending(store, DurableExecutionReceiver(store)))
        lease = store.claim_ready_node("worker", uuid4())
        sink = ArtifactCheckpointSink(
            store, LocalArtifactStore(tmp_path / "invalid-artifacts"), "worker",
        )
        with pytest.raises(ValueError, match="must evaluate to a boolean"):
            asyncio.run(sink.persist_model_result(
                claim_id=lease.claim_id,
                node_run_id=lease.node_run_id,
                response=ModelResponse(
                    text='{"decision":"maybe"}', provider="test", model="test",
                ),
            ))
        nodes = {item.node_id: item for item in store.list_node_runs(receipt.run_id)}
        assert nodes["route"].status.value == "failed"
        assert nodes["route"].error_code == "routing_condition_invalid"
        assert nodes["next"].status.value == "pending"
        assert store.get_run(receipt.run_id).status.value == "failed"
        assert store.list_edge_decisions(receipt.run_id) == []
    finally:
        store.close()
