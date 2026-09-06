"""Mechanical anti-drift gate: green path plus every failure mode.

Corruption is injected with direct SQL (test-only); the checker itself is
read-only. Production wiring (prompt resolver refusal) is covered by the
final test in this module.
"""

import asyncio
import json
from uuid import uuid4

import pytest

sa = pytest.importorskip("sqlalchemy")

from anchor.domain.admission import RunRequest
from anchor.domain.graph import GraphDefinition, GraphEdge, GraphNode, GraphVersion
from anchor.runtime.artifacts import LocalArtifactStore
from anchor.runtime.dispatch import dispatch_pending
from anchor.runtime.integrity import IntegrityError, check_run, require_clean
from anchor.runtime.receiver import DurableExecutionReceiver
from conftest import make_store


def seed_two_node(tmp_path, name="chain.sqlite"):
    store = make_store(tmp_path, name)
    artifacts = LocalArtifactStore(tmp_path / "artifacts")
    definition = GraphDefinition(
        graph_id="chain", name="Chain",
        nodes=[GraphNode(id="a", type="agent", name="A", agent_ref="agents.a"),
               GraphNode(id="b", type="agent", name="B", agent_ref="agents.b")],
        edges=[GraphEdge(source="a", target="b")],
    )
    store.publish_graph(GraphVersion.publish(definition, 1))
    return store, artifacts


def admit(store, inputs=None):
    from anchor.state.relational import RelationalStateStore  # noqa (type clarity)
    assert isinstance(store, RelationalStateStore)
    versions = store.engine.connect().execute(
        sa.text("SELECT graph_version_id FROM graph_versions LIMIT 1")).fetchone()
    import uuid as uuid_mod
    from anchor.domain.graph import Trigger as TriggerModel
    trigger = store.create_trigger(TriggerModel(
        graph_version_id=uuid_mod.UUID(versions[0]), type="manual"))
    receipt = store.admit_run(RunRequest(trigger_id=trigger.id, idempotency_key=f"k-{uuid4().hex}",
                                         objective="integrity", inputs=inputs or {}))
    asyncio.run(dispatch_pending(store, DurableExecutionReceiver(store)))
    return receipt


def complete_all(store, artifacts):
    nodes = {}
    for _ in range(2):
        lease = store.claim_ready_node("worker", uuid4())
        assert lease is not None
        ref = artifacts.put_text(f"content-{lease.node_id}")
        store.complete_node_and_propagate(lease.claim_id, "worker", output_ref=ref,
                                          input_snapshot={"inputs": {"n": lease.node_id}})
        nodes[lease.node_id] = ref
    return nodes


def codes(issues):
    return [issue.code for issue in issues]


def test_clean_run_passes_all_checks(tmp_path):
    store, artifacts = seed_two_node(tmp_path)
    try:
        receipt = admit(store)
        complete_all(store, artifacts)
        assert check_run(store, receipt.run_id, read_artifact=artifacts.get_text) == ()
        require_clean(store, receipt.run_id, read_artifact=artifacts.get_text)
    finally:
        store.close()


def test_unknown_run_is_reported(tmp_path):
    store, _ = seed_two_node(tmp_path)
    try:
        assert codes(check_run(store, uuid4())) == ["unknown_run"]
    finally:
        store.close()


def test_snapshot_hash_mismatch_is_detected(tmp_path):
    store, artifacts = seed_two_node(tmp_path)
    try:
        receipt = admit(store)
        complete_all(store, artifacts)
        with store.engine.begin() as connection:
            connection.execute(sa.text(
                "UPDATE context_snapshots SET snapshot = :s WHERE generation = 1"),
                {"s": json.dumps({"inputs": {"tampered": True}})})
        assert "snapshot_hash_mismatch" in codes(check_run(store, receipt.run_id))
    finally:
        store.close()


def test_generation_gap_is_detected(tmp_path):
    store, artifacts = seed_two_node(tmp_path)
    try:
        receipt = admit(store)
        complete_all(store, artifacts)
        with store.engine.begin() as connection:
            connection.execute(sa.text("DELETE FROM context_snapshots WHERE generation = 1"))
        found = codes(check_run(store, receipt.run_id, read_artifact=artifacts.get_text))
        assert "generation_gap" in found
    finally:
        store.close()


def test_stale_run_generation_is_detected(tmp_path):
    store, artifacts = seed_two_node(tmp_path)
    try:
        receipt = admit(store)
        complete_all(store, artifacts)
        with store.engine.begin() as connection:
            connection.execute(sa.text("UPDATE runs SET context_generation = 99"))
        assert "generation_stale" in codes(check_run(store, receipt.run_id))
    finally:
        store.close()


def test_decision_pin_mismatch_is_detected(tmp_path):
    store, artifacts = seed_two_node(tmp_path)
    try:
        receipt = admit(store)
        complete_all(store, artifacts)
        with store.engine.begin() as connection:
            connection.execute(sa.text(
                "UPDATE edge_decisions SET target_node_id = 'elsewhere'"))
        assert "decision_pin_mismatch" in codes(check_run(store, receipt.run_id))
    finally:
        store.close()


def test_missing_evidence_is_detected(tmp_path):
    store, _ = seed_two_node(tmp_path)
    try:
        receipt = admit(store)
        lease = store.claim_ready_node("worker", uuid4())
        store.complete_node_and_propagate(lease.claim_id, "worker",
                                          output_ref="artifact://sha256/" + "f" * 64,
                                          input_snapshot={"inputs": {}})
        ghost = LocalArtifactStore(tmp_path / "empty-artifacts")
        found = codes(check_run(store, receipt.run_id, read_artifact=ghost.get_text))
        assert "evidence_missing" in found
    finally:
        store.close()


def test_fresh_admitted_run_is_clean(tmp_path):
    # An admitted-but-unexecuted run has no snapshots yet; the gate must
    # still describe state, not crash. Empty generations are dense.
    store, _ = seed_two_node(tmp_path)
    try:
        receipt = admit(store)
        require_clean(store, receipt.run_id)
    finally:
        store.close()


def test_require_clean_raises_with_codes(tmp_path):
    store, artifacts = seed_two_node(tmp_path)
    try:
        receipt = admit(store)
        complete_all(store, artifacts)
        with store.engine.begin() as connection:
            connection.execute(sa.text("DELETE FROM context_snapshots WHERE generation = 1"))
        with pytest.raises(IntegrityError) as excinfo:
            require_clean(store, receipt.run_id)
        assert "generation_gap" in str(excinfo.value)
    finally:
        store.close()


def test_prompt_resolver_refuses_corrupt_context(tmp_path):
    from anchor.runtime.worker_service import _resolver

    store, artifacts = seed_two_node(tmp_path)
    try:
        receipt = admit(store)
        complete_all(store, artifacts)
        with store.engine.begin() as connection:
            connection.execute(sa.text(
                "UPDATE context_snapshots SET snapshot = :s WHERE generation = 1"),
                {"s": json.dumps({"inputs": {"tampered": True}})})
        with pytest.raises(IntegrityError) as excinfo:
            asyncio.run(_resolver(store, receipt.run_id, "b", artifacts=artifacts))
        assert "snapshot_hash_mismatch" in str(excinfo.value)
    finally:
        store.close()
