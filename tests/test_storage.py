"""Read-only storage footprint accounting for the retention budget.

Per-graph occupancy is measured on artifacts because they dominate disk use and
are the most variable component. Artifacts are content addressed and shared
across runs and graphs, so exclusive and shared bytes are reported separately:
a shared blob is never charged to a single graph's budget.
"""

import asyncio
import pytest
from uuid import uuid4

from anchor.domain.admission import RunRequest
from anchor.domain.graph import GraphDefinition, GraphNode, GraphVersion, Trigger
from anchor.runtime.artifacts import LocalArtifactStore
from anchor.runtime.dispatch import dispatch_pending
from anchor.runtime.receiver import DurableExecutionReceiver
from anchor.state.storage import artifact_sizes, storage_report
from conftest import make_store


def completed_run(store, graph_id, output_ref, key):
    version = store.publish_graph(GraphVersion.publish(GraphDefinition(
        graph_id=graph_id, name=graph_id,
        nodes=[GraphNode(id="a", type="agent", name="A", agent_ref="a")]), 1))
    trigger = store.create_trigger(Trigger(graph_version_id=version.graph_version_id,
                                           type="manual"))
    receipt = store.admit_run(RunRequest(trigger_id=trigger.id, idempotency_key=key,
                                         objective="storage", inputs={}))
    asyncio.run(dispatch_pending(store, DurableExecutionReceiver(store)))
    lease = store.claim_ready_node("worker", uuid4())
    store.complete_node_and_propagate(lease.claim_id, "worker", output_ref=output_ref,
                                      input_snapshot={"inputs": {}})
    return receipt


def test_storage_report_separates_exclusive_and_shared_artifacts(tmp_path):
    store = make_store(tmp_path, "storage.sqlite")
    artifacts = LocalArtifactStore(tmp_path / "artifacts")
    try:
        shared = artifacts.put_text("shared evidence")
        exclusive = artifacts.put_text("only this graph")
        completed_run(store, "graph-a", shared, "storage-a")
        completed_run(store, "graph-b", shared, "storage-b")
        completed_run(store, "graph-b", exclusive, "storage-c")

        report = storage_report(store, artifact_root=tmp_path / "artifacts")
        sizes = artifact_sizes(tmp_path / "artifacts")
        assert report["artifacts_files"] == 2
        assert report["artifacts_bytes"] == sum(sizes.values())
        assert report["database_bytes"] and report["database_bytes"] > 0
        assert report["total_bytes"] == report["database_bytes"] + report["artifacts_bytes"]
        assert report["runs_total"] == 3 and report["runs_terminal"] == 3

        graphs = {item["graph_id"]: item for item in report["graphs"]}
        shared_bytes = sizes[shared]
        exclusive_bytes = sizes[exclusive]

        assert graphs["graph-a"]["runs"] == 1
        assert graphs["graph-a"]["exclusive_bytes"] == 0
        assert graphs["graph-a"]["shared_bytes"] == shared_bytes
        assert graphs["graph-b"]["runs"] == 2
        assert graphs["graph-b"]["exclusive_bytes"] == exclusive_bytes
        assert graphs["graph-b"]["shared_bytes"] == shared_bytes
        assert graphs["graph-b"]["artifact_bytes"] == exclusive_bytes + shared_bytes

        # Budgets are advisory here; the report only flags them.
        assert report["over_global_budget"] is False
        over = storage_report(store, artifact_root=tmp_path / "artifacts",
                              global_budget=1,
                              graph_budgets={"graph-a": 1, "graph-b": 1})
        assert over["over_global_budget"] is True
        assert all(item["over_budget"] for item in over["graphs"])
    finally:
        store.close()


def test_storage_report_is_empty_and_safe_on_a_fresh_store(tmp_path):
    store = make_store(tmp_path, "empty.sqlite")
    try:
        report = storage_report(store, artifact_root=tmp_path / "missing")
        assert report["artifacts_files"] == 0
        assert report["graphs"] == []
        assert report["runs_total"] == 0
    finally:
        store.close()


def test_storage_budgets_are_persisted_and_clearable(tmp_path):
    store = make_store(tmp_path, "budgets.sqlite")
    try:
        assert store.get_storage_budgets() == {}
        store.set_storage_budget(store.GLOBAL_SCOPE, 1024)
        store.set_storage_budget("graph-a", 2048)
        assert store.get_storage_budgets() == {"__global__": 1024, "graph-a": 2048}

        # Upsert and clear.
        store.set_storage_budget("graph-a", 4096)
        assert store.get_storage_budgets()["graph-a"] == 4096
        store.set_storage_budget("graph-a", None)
        assert "graph-a" not in store.get_storage_budgets()
        assert store.get_storage_budgets()["__global__"] == 1024

        with pytest.raises(ValueError, match="negative"):
            store.set_storage_budget("graph-a", -1)

        # 1024 bytes cannot even hold an empty SQLite file, so this is over.
        report = storage_report(store, artifact_root=tmp_path / "missing",
                                global_budget=store.get_storage_budgets()["__global__"])
        assert report["budget"] == {"global_bytes": 1024}
        assert report["over_global_budget"] is True
        roomy = storage_report(store, artifact_root=tmp_path / "missing",
                               global_budget=10 * 1024 ** 3)
        assert roomy["over_global_budget"] is False
    finally:
        store.close()
