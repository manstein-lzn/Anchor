"""Rolling retention: finished runs are evicted oldest-first, artifacts reclaimed.

The sweep is the only destructive path, so these tests pin its boundaries: no
budget means no deletion, protected runs survive, shared artifacts are only
removed once nothing references them, and every eviction is audited.
"""

import asyncio
from uuid import uuid4

from anchor.domain.admission import RunRequest
from anchor.domain.graph import GraphDefinition, GraphNode, GraphVersion, Trigger
from anchor.runtime.artifacts import LocalArtifactStore
from anchor.runtime.dispatch import dispatch_pending
from anchor.runtime.receiver import DurableExecutionReceiver
from anchor.runtime.retention import enforce_storage_budgets, plan_storage_budgets
from anchor.state.storage import artifact_sizes
from conftest import make_store


def setup(tmp_path, count, name="retention.sqlite"):
    store = make_store(tmp_path, name)
    artifacts = LocalArtifactStore(tmp_path / "artifacts")
    version = store.publish_graph(GraphVersion.publish(GraphDefinition(
        graph_id="retention-graph", name="Retention",
        nodes=[GraphNode(id="a", type="agent", name="A", agent_ref="a")]), 1))
    trigger = store.create_trigger(Trigger(graph_version_id=version.graph_version_id,
                                           type="manual"))
    runs = []
    for index in range(count):
        receipt = store.admit_run(RunRequest(
            trigger_id=trigger.id, idempotency_key=f"retention-{uuid4().hex}",
            objective=f"run {index}", inputs={}))
        asyncio.run(dispatch_pending(store, DurableExecutionReceiver(store)))
        lease = store.claim_ready_node("worker", uuid4())
        ref = artifacts.put_text(f"output {index}")
        store.complete_node_and_propagate(lease.claim_id, "worker", output_ref=ref,
                                          input_snapshot={"inputs": {}})
        runs.append((receipt.run_id, ref))
    return store, artifacts, runs


def test_no_budget_never_deletes_anything(tmp_path):
    store, artifacts, runs = setup(tmp_path, 2)
    try:
        result = enforce_storage_budgets(store, artifacts)
        assert result["reason"] == "no_budget" and result["evicted"] == 0
        assert len(store.list_runs(include_archived=True)) == 2
        assert len(artifact_sizes(artifacts.root)) == 2
        assert store.list_retention_audit() == []
    finally:
        store.close()


def test_rolling_sweep_evicts_finished_runs_and_reclaims_their_artifacts(tmp_path):
    store, artifacts, runs = setup(tmp_path, 3)
    try:
        store.set_storage_budget(store.GLOBAL_SCOPE, 1)  # cannot hold anything
        result = enforce_storage_budgets(store, artifacts, trigger="test")
        assert result["evicted"] == 3
        assert store.list_runs(include_archived=True) == []
        assert artifact_sizes(artifacts.root) == {}
        audit = store.list_retention_audit()
        assert len(audit) == 1 and audit[0]["evicted_runs"] == 3 and audit[0]["trigger"] == "test"
        assert audit[0]["freed_bytes"] > 0
        for run_id, _ in runs:
            assert run_id not in {item.id for item in store.list_runs(include_archived=True)}
    finally:
        store.close()


def test_sweep_never_touches_a_run_that_still_needs_work_or_a_human(tmp_path):
    store, artifacts, runs = setup(tmp_path, 1)
    try:
        # A second run stays non-terminal, so it is protected.
        version = store.list_graph_versions("retention-graph")[0]
        trigger = store.list_triggers(version.graph_version_id)[0]
        pending = store.admit_run(RunRequest(trigger_id=trigger.id,
                                             idempotency_key=f"pending-{uuid4().hex}",
                                             objective="still open", inputs={}))
        store.set_storage_budget(store.GLOBAL_SCOPE, 1)
        result = enforce_storage_budgets(store, artifacts, trigger="test")
        assert result["evicted"] == 1
        remaining = {item.id for item in store.list_runs(include_archived=True)}
        assert pending.run_id in remaining and runs[0][0] not in remaining
    finally:
        store.close()


def test_shared_artifact_survives_until_the_last_referrer_is_evicted(tmp_path):
    store, artifacts, _ = setup(tmp_path, 1)
    try:
        shared = artifacts.put_text("shared body")
        version = store.list_graph_versions("retention-graph")[0]
        trigger = store.list_triggers(version.graph_version_id)[0]
        receipt = store.admit_run(RunRequest(trigger_id=trigger.id,
                                             idempotency_key=f"shared-{uuid4().hex}",
                                             objective="shares", inputs={}))
        asyncio.run(dispatch_pending(store, DurableExecutionReceiver(store)))
        lease = store.claim_ready_node("worker", uuid4())
        store.complete_node_and_propagate(lease.claim_id, "worker", output_ref=shared,
                                          input_snapshot={"inputs": {}})

        # Evict the older run only; the shared blob is still referenced.
        plan = plan_storage_budgets(store, artifacts)
        oldest = plan["candidates"][0]["run_id"]
        store.purge_run(oldest)
        from anchor.runtime.retention import collect_garbage
        collect_garbage(store, artifacts)
        assert shared in artifact_sizes(artifacts.root)

        # Evicting the last referrer removes it.
        store.purge_run(receipt.run_id)
        collect_garbage(store, artifacts)
        assert shared not in artifact_sizes(artifacts.root)
    finally:
        store.close()


def test_plan_lists_candidates_oldest_first_and_skips_protected(tmp_path):
    store, artifacts, runs = setup(tmp_path, 2)
    try:
        store.set_storage_budget(store.GLOBAL_SCOPE, 1)
        plan = plan_storage_budgets(store, artifacts)
        assert plan["needed"] is True and plan["over_global_budget"] is True
        order = [item["run_id"] for item in plan["candidates"]]
        assert order == [str(runs[0][0]), str(runs[1][0])]
        assert plan["protected_runs"] == 0
    finally:
        store.close()
