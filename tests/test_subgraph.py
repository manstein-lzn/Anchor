"""Subgraph composition by publish-time materialization.

A `subgraph` node pins one immutable Graph Version. Publication expands the
pinned child inline with namespaced IDs, so execution uses the existing
machinery unchanged. These tests pin the reference rules: unknown/invalid
refs fail, cycles are rejected, pins are stable, mappings are rewritten,
and an expanded parent runs to terminal with store-level workers only.
"""

import asyncio
import json
from uuid import UUID, uuid4

import pytest

from anchor.domain.graph import (
    SUBGRAPH_AUTHORING_HASH_KEY,
    SUBGRAPH_EXPANSION_KEY,
    GraphDefinition,
    GraphEdge,
    GraphNode,
    GraphVersion,
    NodeType,
    expand_subgraphs,
)
from anchor.runtime.artifacts import LocalArtifactStore
from anchor.runtime.dispatch import dispatch_pending
from anchor.runtime.receiver import DurableExecutionReceiver
from anchor.domain.admission import RunRequest
from anchor.domain.graph import Trigger
from conftest import make_store


def agent(node_id, ref="agents.a"):
    return GraphNode(id=node_id, type=NodeType.AGENT, name=node_id, agent_ref=ref)


def publish_child(store):
    child = GraphDefinition(
        graph_id="child", name="Child",
        nodes=[agent("x"), agent("y")],
        edges=[GraphEdge(source="x", target="y")],
    )
    store.save_draft("child", expected_revision=0,
                     definition=child.model_dump(mode="json"), layout={})
    return store.publish_draft("child", expected_revision=1)


def publish_parent(store, child_version_id, **overrides):
    nodes = [agent("a"), GraphNode(id="s", type=NodeType.SUBGRAPH, name="s",
                                   subgraph_version_id=str(child_version_id)),
             agent("b")]
    edges = [GraphEdge(source="a", target="s"), GraphEdge(source="s", target="b")]
    definition = GraphDefinition(graph_id="parent", name="Parent",
                                 nodes=overrides.get("nodes", nodes),
                                 edges=overrides.get("edges", edges),
                                 entry_node_id=overrides.get("entry_node_id", "a"))
    store.save_draft("parent", expected_revision=0,
                     definition=definition.model_dump(mode="json"), layout={})
    return store.publish_draft("parent", expected_revision=1)


def test_unknown_subgraph_version_fails_closed(tmp_path):
    store = make_store(tmp_path)
    try:
        with pytest.raises(ValueError, match="unknown subgraph version"):
            publish_parent(store, uuid4())
    finally:
        store.close()


def test_invalid_subgraph_version_id_fails_closed(tmp_path):
    store = make_store(tmp_path)
    try:
        child = publish_child(store)
        assert child is not None
        store.save_draft("bad", expected_revision=0, definition=GraphDefinition(
            graph_id="bad", name="Bad",
            nodes=[GraphNode(id="s", type=NodeType.SUBGRAPH, name="s",
                             subgraph_version_id="not-a-uuid")],
        ).model_dump(mode="json"), layout={})
        with pytest.raises(ValueError, match="invalid subgraph_version_id"):
            store.publish_draft("bad", expected_revision=1)
    finally:
        store.close()


def test_subgraph_node_without_ref_is_rejected(tmp_path):
    store = make_store(tmp_path)
    try:
        store.save_draft("noref", expected_revision=0, definition={
            "graph_id": "noref", "name": "NoRef",
            "nodes": [{"id": "s", "type": "subgraph", "name": "s"}],
            "edges": [],
        }, layout={})
        with pytest.raises(ValueError, match="requires subgraph_version_id"):
            store.publish_draft("noref", expected_revision=1)
    finally:
        store.close()


def test_reference_cycle_is_rejected_by_expansion():
    left = GraphDefinition(graph_id="l", name="L", nodes=[
        GraphNode(id="s", type=NodeType.SUBGRAPH, name="s",
                  subgraph_version_id="11111111-1111-1111-1111-111111111111")])
    right = GraphDefinition(graph_id="r", name="R", nodes=[
        GraphNode(id="s", type=NodeType.SUBGRAPH, name="s",
                  subgraph_version_id="22222222-2222-2222-2222-222222222222")])
    versions = {"11111111-1111-1111-1111-111111111111": left,
                "22222222-2222-2222-2222-222222222222": right}

    def resolve(version_id):
        return versions[version_id]

    with pytest.raises(ValueError, match="subgraph cycle"):
        expand_subgraphs(left, resolve)


def test_expansion_shape_provenance_and_no_subgraph_nodes(tmp_path):
    import sqlalchemy as sa
    from uuid import UUID as UUID_
    store = make_store(tmp_path)
    try:
        child_id = publish_child(store).graph_version_id
        publish_parent(store, child_id)
        row = store.engine.connect().execute(sa.text(
            "SELECT graph_version_id FROM graph_versions WHERE graph_id='parent'"
        )).fetchone()
        assert row is not None
        parent = store.get_graph_version(UUID_(row[0]))
        assert parent is not None
        ids = sorted(node.id for node in parent.definition.nodes)
        assert ids == ["a", "b", "s__x", "s__y"]
        assert all(node.type is not NodeType.SUBGRAPH for node in parent.definition.nodes)
        edges = {(edge.source, edge.target) for edge in parent.definition.edges}
        assert edges == {("a", "s__x"), ("s__x", "s__y"), ("s__y", "b")}
        assert parent.definition.entry_node_id == "a"
        provenance = json.loads(parent.definition.metadata[SUBGRAPH_EXPANSION_KEY])
        assert provenance == [{"site": "s", "child_version": str(child_id),
                                "prefix": "s__"}]
        assert len(parent.definition.metadata[SUBGRAPH_AUTHORING_HASH_KEY]) == 64
        # The stored version validates as an ordinary executable graph.
        republished = GraphVersion.publish(parent.definition, parent.version)
        assert republished.content_hash == parent.content_hash
    finally:
        store.close()


def test_input_mapping_is_rewritten_across_boundary(tmp_path):
    store = make_store(tmp_path)
    try:
        child_id = publish_child(store).graph_version_id
        parent = publish_parent(
            store, child_id,
            edges=[GraphEdge(source="a", target="s"),
                   GraphEdge(source="s", target="b",
                             input_mapping={"evidence": "outputs.y"})])
        rewritten = [edge for edge in parent.definition.edges if edge.source == "s__y"]
        assert len(rewritten) == 1
        assert rewritten[0].input_mapping == {"evidence": "outputs.s__y"}
    finally:
        store.close()


def test_pin_is_stable_when_child_republishes(tmp_path):
    store = make_store(tmp_path)
    try:
        first_child = publish_child(store)
        parent = publish_parent(store, first_child.graph_version_id)
        before = parent.content_hash
        changed = GraphDefinition(
            graph_id="child", name="Child",
            nodes=[agent("x"), agent("y"), agent("z")],
            edges=[GraphEdge(source="x", target="y"), GraphEdge(source="y", target="z")],
        )
        store.save_draft("child", expected_revision=1,
                         definition=changed.model_dump(mode="json"), layout={})
        second_child = store.publish_draft("child", expected_revision=2)
        assert second_child.graph_version_id != first_child.graph_version_id
        same = store.get_graph_version(parent.graph_version_id)
        assert same is not None and same.content_hash == before
        assert sorted(node.id for node in same.definition.nodes) == ["a", "b", "s__x", "s__y"]
    finally:
        store.close()


def test_expanded_parent_runs_to_terminal_with_existing_machinery(tmp_path):
    store = make_store(tmp_path)
    try:
        parent = publish_parent(store, publish_child(store).graph_version_id)
        artifacts = LocalArtifactStore(tmp_path / "artifacts")
        trigger = store.create_trigger(Trigger(graph_version_id=parent.graph_version_id,
                                               type="manual"))
        receipt = store.admit_run(RunRequest(
            trigger_id=trigger.id, idempotency_key="subgraph-run",
            objective="composed", inputs={}))
        asyncio.run(dispatch_pending(store, DurableExecutionReceiver(store)))
        for _ in range(4):
            lease = store.claim_ready_node("worker", uuid4())
            assert lease is not None
            ref = artifacts.put_text(f"out-{lease.node_id}")
            store.complete_node_and_propagate(lease.claim_id, "worker", output_ref=ref,
                                              input_snapshot={"inputs": {}})
        run = store.get_run(receipt.run_id)
        assert run is not None and run.status.value == "completed"
        assert sorted(node.node_id for node in store.list_node_runs(receipt.run_id)) == [
            "a", "b", "s__x", "s__y"]
    finally:
        store.close()
