from uuid import uuid4
from anchor.runtime.memory import LocalMemoryStore, MemoryRecord

def test_memory_has_provenance_and_tombstone_delete(tmp_path):
    run_id=uuid4(); store=LocalMemoryStore(tmp_path/"memory.jsonl")
    record=store.put(MemoryRecord.create("important fact", run_id=run_id))
    assert store.list(run_id=run_id)[0].content == "important fact"
    deleted=store.delete(record.memory_id)
    assert deleted.deleted_at is not None and store.list(run_id=run_id) == []
    assert len(store.list(run_id=run_id, include_deleted=True)) == 1
    assert store.purge_deleted() == 1
    assert store.list(run_id=run_id, include_deleted=True) == []

def test_lesson_proposal_review_and_promotion(tmp_path):
    from anchor.runtime.memory import MemoryRecord as MR
    store = LocalMemoryStore(tmp_path/"memory.jsonl")
    proposed = store.put(MR.propose("always verify artifact hashes", domain="evidence"))
    assert proposed.status == "proposed"
    assert store.list(status="promoted") == []
    promoted = store.review(proposed.memory_id, status="promoted",
                            reviewer="owner", reason="confirmed in E2E")
    assert promoted.status == "promoted" and promoted.reviewed_by == "owner"
    assert [item.content for item in store.list(status="promoted")] == ["always verify artifact hashes"]
    second = store.put(MR.propose("skip checks when rushed", domain="evidence"))
    store.review(second.memory_id, status="rejected", reviewer="owner", reason="unsafe")
    assert [item.content for item in store.list(status="promoted")] == ["always verify artifact hashes"]


def test_review_rejects_bad_transitions(tmp_path):
    import pytest as _pytest
    store = LocalMemoryStore(tmp_path/"memory.jsonl")
    active = store.put(MemoryRecord.create("working note"))
    with _pytest.raises(ValueError, match="only proposed"):
        store.review(active.memory_id, status="promoted", reviewer="o", reason="r")
    with _pytest.raises(ValueError, match="promote or reject"):
        store.review(active.memory_id, status="active", reviewer="o", reason="r")
    with _pytest.raises(KeyError):
        from uuid import uuid4 as _uuid4
        store.review(_uuid4(), status="promoted", reviewer="o", reason="r")


def test_promoted_knowledge_enters_future_prompts(tmp_path):
    import asyncio
    from anchor.runtime.worker_service import _resolver
    from anchor.domain.admission import RunRequest
    from anchor.domain.graph import GraphDefinition, GraphNode, GraphVersion, Trigger
    from anchor.runtime.dispatch import dispatch_pending
    from anchor.runtime.receiver import DurableExecutionReceiver
    from uuid import uuid4 as _uuid4
    from conftest import make_store
    store = make_store(tmp_path, "promo.sqlite")
    try:
        memory = LocalMemoryStore(tmp_path/"memory.jsonl")
        candidate = memory.put(MemoryRecord.propose("verify hashes first", domain="evidence"))
        memory.review(candidate.memory_id, status="promoted", reviewer="o", reason="ok")
        definition = GraphDefinition(
            graph_id="promo", name="Promo",
            nodes=[GraphNode(id="a", type="agent", name="A", agent_ref="agents.a")])
        store.publish_graph(GraphVersion.publish(definition, 1))
        version = store.list_graph_versions("promo")[0]
        trigger = store.create_trigger(Trigger(graph_version_id=version.graph_version_id, type="manual"))
        receipt = store.admit_run(RunRequest(trigger_id=trigger.id, idempotency_key="promo-1",
                                             objective="promo", inputs={}))
        asyncio.run(dispatch_pending(store, DurableExecutionReceiver(store)))
        resolved = asyncio.run(_resolver(store, receipt.run_id, "a", memory=memory))
        assert "[evidence] verify hashes first" in resolved.prompt
        assert "Promoted organizational knowledge" in resolved.prompt
    finally:
        store.close()
